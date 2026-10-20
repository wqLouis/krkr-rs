# `null_plugin.rs` — the null plugin object

**Source:** `crates/compat/src/null_plugin.rs`

Linux and Android cannot load the Windows plugin DLLs the games name.
`Plugins.link(name)` therefore either activates a built-in emulation or
installs a **null plugin object** that accepts every member access, does
nothing, logs a warning and **never throws**.

This is the single documented exception to the no-silent-fallback rule, and
it is **not** silent: every member access logs a warning.

---

## 1. The mechanism

`tjs`'s class builder can only register **named** members; there is no
wildcard or proxy class. The closest mechanism the ABI supports is the TJS
**`missing` member handler**: a native instance class registers a
`missing(getorset, name, value)` method and enables `CallMissing` on each
instance, after which every absent member resolves to `missing` instead of
raising.

The module therefore registers a `NullPlugin` native instance class:

| Member | Behaviour |
|---|---|
| `NullPlugin` (the class-name constructor) | records the plugin name in the payload and enables `CallMissing` on the object |
| `missing(getorset, name, value)` | logs the access, hands back a no-op callable for a get, ignores a set, and reports the access as handled |

The class name is the VM-visible `NullPlugin` (`NULL_PLUGIN_CLASS`).

### What it catches, and what it cannot

* **Property reads and writes never throw.** A read returns a no-op
  callable; a write is ignored.
* **Method calls are intercepted too.** A get is also how the VM resolves a
  *call* to an absent member, so the property object's default member is set
  (through the `tjs2_prop_set_default` ABI) to a no-op closure: `plugin.foo()`
  runs the no-op instead of raising. A plain read returns that same closure.
* **Unknown class names** (`new SomePluginClass()`) are global lookups, not
  member accesses on the null object, so they are **not** caught.

---

## 2. The no-op callable

The no-op closure is created **once** at install time
(`CompatServices::ensure_noop_closure`) and **duplicated** per access, so:

* the hot path never re-enters the VM;
* the engine's "last object" slot is never clobbered;
* a member read is a duplication, not a compile.

The cached id is stored as a raw `Tjs2ValueId` and the Rust handle is
deliberately leaked: the engine owns the C++ retention and frees it at
destruction, so dropping the host can never touch the engine. See
[`ctx.md`](ctx.md) §1.

If the closure cannot be created or duplicated, the access has already been
logged — the failure is non-fatal and the member still resolves.

---

## 3. Installing the global

`install(ctx, plugin_name) -> Result<bool, CompatError>` creates one instance
per missing plugin and installs it as a script global under the plugin's
**stem** (`Plugins.link("windowEx.dll")` → `global.windowEx`).

### The stem is never clobbered

The probe uses `typeof`, because it is the one read that cannot raise for an
absent member: the VM maps a failed property lookup to the string
`"undefined"`, whereas a plain read of an absent `global[stem]` raises
member-not-found.

| `typeof global[stem]` | Outcome |
|---|---|
| `"undefined"` or `"void"` | the null object is installed — returns `Ok(true)` |
| anything else | an existing binding is **kept**, with a warning — returns `Ok(false)` |

A member that exists but holds `void` counts as absent too: a script may
clear it by hand. `plugin_name` keeps the script's original casing, so the
global is `windowEx`, not `windowex`.

The link is recorded **either way**; only the owner of the binding may clear
it later.

### The assignment

```tjs
global[stem] = new NullPlugin(plugin_name)
```

The expression is evaluated with `eval_retained` (which compiles an
*expression*, so the assignment yields the object itself — an `exec_script`
statement would yield void). Only the script global holds the object from
then on; the retention the expression produced is released.

A `RetainedValue::Value` result (the constructor did not return an object) is
a typed error.

---

## 4. Removing the global

`remove(ctx, plugin_name)` clears the null plugin's global, but **only when
it still is the null object this module installed**:

| `typeof global[stem]` | Outcome |
|---|---|
| `"undefined"` or `"void"` | nothing there — already removed or cleared by a script |
| `"Object"` | the `instanceof` guard decides |
| anything else | a scalar now holds the global — it is left alone (debug log) |

The `instanceof` check runs inside a `try`/`catch` in the evaluated script:
`instanceof` can raise for an object without a class chain (a function the
script put in our place), where the reference's `IsInstanceOf` simply reports
false. A raised check therefore leaves the binding alone rather than deleting
someone else's global.

The object's native class cannot be unregistered either way; a later `link`
re-installs the null object when the stem is free again.

---

## 5. The logging

| Level | Message |
|---|---|
| `warn` | `null plugin {name:?}: ignoring member {member:?} (getorset={get_or_set})` — one per member access |
| `debug` | `null plugin {name:?}: missing(getorset={get_or_set}, name={member:?}, value={value:?})` |
| `warn` | `null plugin {plugin_name:?}: global {stem:?} already holds a value of type {kind}; keeping it instead of installing the null object` |
| `debug` | `null plugin {plugin_name:?}: installed global {stem:?}` |
| `debug` | `null plugin {plugin_name:?}: global {stem:?} now holds a {scalar}; leaving it` |

The warning is emitted **per call**, which is what makes the exception
non-silent: a game that keeps calling an unported member keeps producing
warnings that name the member.

---

## 6. The string literal helper

`null_plugin` builds TJS string literals itself rather than passing a value
through the ABI. Control characters are emitted as four-digit `\xHHHH`
escapes: the lexer consumes up to `sizeof(tjs_char)*2` hex digits, so a fixed
width keeps a following hex digit from being absorbed into the escape.

---

## 7. Errors

| Error | Raised when |
|---|---|
| `CompatError::Register("null plugin: typeof global[{stem:?}] did not yield a type name")` | the `typeof` probe did not return a string |
| `CompatError::Register("null plugin: the constructor did not return an object")` | the constructor expression yielded a non-object |
| `CompatError::Register(err)` | the evaluated script failed |

The `missing` handler itself never fails: it logs and returns `true` (the
access was handled), so the VM does not raise.

---

## 8. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/null_plugin.rs` | an unported name installs the object under its stem; a member read returns a callable; a member call runs the no-op; a write is ignored; the warning names the member |
| `crates/compat/tests/null_plugin.rs` | an existing `global[stem]` is kept, not clobbered, and `unlink` leaves it alone |
| `crates/compat/tests/plugin.rs` | `link` on an unported name returns the null outcome and records the link |
| `crates/compat/tests/menu_dll_surface.rs` | `menu.dll` links as a built-in, so **no** null global is installed |

---

## See also

* [`plugin.md`](plugin.md) — the registry that decides built-in vs null
* [`plugins_native.md`](plugins_native.md) — the `Plugins` class
* [`../../arch/l3.md`](../../arch/l3.md) §5 — the null plugin at the layer level
* [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §8 — the plugin rules

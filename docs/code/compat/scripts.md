# `compat::natives::scripts` — the `Scripts` class

Source: `crates/compat/src/natives/scripts.rs` · layer **L3** · class `Scripts`

`Scripts` is the script's execution surface: it runs and evaluates script
text and storage files, compiles storage to bytecode, inspects the VM
(class names, stack trace) and drives the call-missing handler.

---

## 1. Registered surface

All members are static.

| member | behaviour |
|---|---|
| `execStorage(name[, mode[, context]])` | runs a storage file as a script |
| `evalStorage(name[, mode[, context]])` | evaluates a storage file as an expression |
| `exec(script[, name[, lineofs[, context]]])` | runs a script string |
| `eval(expression[, name[, lineofs[, context]]])` | evaluates an expression string |
| `compileStorage(name, output[, isresult[, outputdebug[, isexpression]]])` | writes a storage file's bytecode to `output` |
| `setCallMissing(obj)` | enables the object's `missing` handler |
| `getClassNames(obj)` | the object's class-name chain, most-derived first |
| `getTraceString([limit])` | the current VM stack trace |
| `dump()` | dumps every live script block |
| `textEncoding` | read/write: the fallback decoder for storage text |

Twenty-two further members are attached to the same class object by the
**scriptsEx plugin**: `clone`, `equalStruct`, `equalStructNumericLoose`,
`evalJSON`, `evalJSONStorage`, `foreach`, `getMD5HashString`,
`getObjectContext`, `getObjectCount`, `getObjectKeys`, `isNullContext`,
`pfHiddenMember`, `pfIgnoreProp`, `pfMemberEnsure`, `pfMemberMustExist`,
`pfStaticMember`, `propGet`, `propSet`, `rehash`, `safeEvalStorage`,
`saveJSON`, `setEvalErrorLog` and `toJSONString`.

---

## 2. Execution

### `execStorage` / `evalStorage`

Both read the storage, apply the mode's `oN` byte offset, and then:

1. offer the bytes to the **bytecode loader** first;
2. only if the loader reports "not bytecode", decode them as text.

The order matters: a file with the bytecode magic that fails to load is a
typed error, not a fall-through to the text path.

`execStorage` runs the script and an explicit top-level `return` is the
result; `evalStorage` evaluates the storage as an expression.

The block name is the **basename** of the requested storage, not the full
name.

The third argument is the script's `this`. It must be an **object**: a
present non-object raises `Scripts.execStorage: the execution context is not
an object` (the reference's `AsObjectNoAddRef` throw). An absent argument is
fine.

### `exec` / `eval`

`exec` compiles and runs the script text; `eval` evaluates an expression.

`lineofs` shifts the reported line numbers. `context` is `this`, with the
same object requirement.

`eval` honours the caller's **result need**, so a resultless expression such
as `swap` is allowed in a statement context and raises
`Cannot get the value of this expression` where a result is required.

### `compileStorage`

Reads `name` as **text** (never as bytecode) and compiles it to a bytecode
file at `output` through the VM's writer. `isresult`, `outputdebug` and
`isexpression` are optional booleans; an absent or void flag is false.

---

## 3. Text decoding

Storage text is decoded by `crate::text`:

1. the `FE FE` cipher/zlib container is detected and unpacked;
2. a BOM selects UTF-32LE/BE, UTF-8 or UTF-16LE/BE;
3. an explicit `Scripts.textEncoding` override is applied;
4. otherwise charset detection runs, preferring CP932 over a Latin-1
   single-byte fallback.

`Scripts.textEncoding` **reads back** the configured spelling — `"UTF-8"`
when nothing was set. The alias forms `sjis` / `shiftjis` / `shift_jis` /
`shift-jis` normalize to `"cp932"`, and `utf8` / `utf-8` to `"UTF-8"`. An
empty string means auto-detection.

Note the deliberate difference: the getter reports `"UTF-8"` while an unset
value internally means "auto-detect", because the reference's
`G_DefaultReadEncoding` is only the last-resort fallback after BOM and
detection.

---

## 4. VM introspection

`getClassNames(obj)` returns the class-name chain as a TJS `Array`,
most-derived first. The argument must be an object.

`getTraceString([limit])` returns the current stack trace as a string. Without
an argument the window is the reference's default; with one it is truncated
to a `tjs_uint`.

`dump()` writes every live script block to the engine log.

`setCallMissing(obj)` enables the object's `missing` member handler. A TJS
`null` is accepted silently (the reference's `if(dsp)` skip); any other
non-object raises.

---

## 5. State ownership

| state | owner |
|---|---|
| the text-encoding spelling, the script-execution nesting depth | the **per-engine dispatch state** (`CompatServices::state::<ScriptsState>()`) |
| the string scratch buffer for returned strings | the **per-engine dispatch state** (a `StringScratch` type local to this module, distinct from the one in `system.rs`) |
| the storage reads | the **L2 storage** |

One entry point is public for the host:

```rust
pub fn exec_storage(engine: &Tjs2Engine, storage: &Storage, name: &str) -> Result<(), String>;
```

The composition root runs the startup sequence through it.

The natives also participate in the startup timing report: the instrumented
members record their dispatch cost, which the host prints once after startup.

---

## 6. Errors

| shape | example |
|---|---|
| `"Scripts.<member> requires at least N argument(s)"` | `Scripts.eval requires at least 1 argument` |
| `"Scripts.<member> requires N argument(s)"` | `Scripts.compileStorage requires at least 2 arguments` |
| `"Scripts.<member>: compat context is not bound"` | the engine binding is missing |
| `"Scripts.<member>: the execution context is not an object"` | a non-object `context` |
| `"Scripts.<member>: no storage attached"` | no L2 storage |
| `"Scripts: '<name>': {error}"` | a storage read or decode failure |
| `"Scripts.<member>: {error}"` | an inner failure (a compile error, a flag conversion) |

`compileStorage`'s storage failure keeps the `Scripts: '<name>': {error}`
shape rather than the member prefix, because it shares the read path with
`execStorage`.

The `textEncoding` **setter** is the one member that can fail **silently**:
when the binding is missing it returns a failure status without writing an
error message.

---

## 7. Tests

`crates/compat/tests/scripts.rs` covers `exec`/`eval` and their results, the
result-need rule, `eval` returning a callable object, `setCallMissing`,
`getClassNames`, `getTraceString` and `dump`, the `textEncoding` round trip
and its alias normalization, `context` as `this` and `lineofs`, the storage
basename as the block name, the debug failure excerpt, the
`compileStorage` → `execStorage` bytecode round trip, the bytecode magic
being rejected by the loader rather than run as text, a corrupt or truncated
bytecode being a typed error, the "no storage" errors and the arity errors.

---

## 8. Known gaps

1. **The bytecode round trip is a self-consistency proof.** The port's
   bytecode *writer* has a known gap — a property-access compound assignment
   (`+=`) is not round-tripped — so a `.tjsb` produced by the original
   KiriKiri is **not** proven to load here. That needs a device run.
2. **Bytecode from another compiler revision is not proven** to load; the
   loader's recognized-but-rejected path raises the loader's own
   `Bytecode read error`.
3. **`compileStorage` reads the source as text only** and always uses an
   empty mode (no `oN` offset).
4. **`textEncoding` unset means auto-detection**, not forced UTF-8, even
   though the getter reports `"UTF-8"`.
5. **The `textEncoding` setter fails silently** when the binding is missing.
6. **Debug-only diagnostics** (not script-observable): the `script read …`
   line and the failure excerpt, which shows the failing line's source window
   capped at 8192 bytes.

---

## 9. Related documents

| topic | document |
|---|---|
| the L3 layer, the plugins and the VM host | [`../../arch/l3.md`](../../arch/l3.md), [`../../arch/l4.md`](../../arch/l4.md) |
| the surface lock | [`../../arch/l3.md`](../../arch/l3.md) §7 |
| the storage namespace | [`storages.md`](storages.md) |
| the KAG parser (which reads scenarios through the same storage) | [`kag.md`](kag.md) |
| the L3 ↔ L2 protocol | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |

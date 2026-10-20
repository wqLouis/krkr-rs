# ScriptsEx (`ScriptsEx.dll`)

**DLL name:** `ScriptsEx.dll` — linked with `Plugins.link("ScriptsEx.dll")`
**Rust module:** `crates/compat/src/natives/plugins/scripts_ex/`
**Reference:** `reference/cpp/plugins/scriptsEx.cpp`
**Registers:** static members on the core `Scripts` class, plus the `pf*` flag constants

## Surface

| member | kind | behaviour |
|---|---|---|
| `Scripts.getObjectKeys(obj)` | static method | the member names as a sorted TJS `Array` |
| `Scripts.getObjectCount(obj)` | static method | the member count (`Dictionary` counted by enumeration) |
| `Scripts.getObjectContext(obj)` | static method | the object's context (`ObjThis`) |
| `Scripts.isNullContext(obj)` | static method | whether that context is null |
| `Scripts.equalStruct(a, b)` | static method | recursive structural equality (`===` leaves) |
| `Scripts.equalStructNumericLoose(a, b)` | static method | as above, but `0 == 0.0` |
| `Scripts.foreach(obj, func, args*)` | static method | iterate; a non-`void` return stops the iteration and is returned |
| `Scripts.getMD5HashString(octet)` | static method | the lowercase hex MD5 of the octet |
| `Scripts.clone(obj)` | static method | a deep copy of arrays and dictionaries; other objects are returned after invoking `clone()` |
| `Scripts.propSet(obj, key, value[, flag])` | static method | `PropSet` / `PropSetByNum` with `flag` (default `TJS_MEMBERENSURE`) |
| `Scripts.propGet(obj, key[, flag])` | static method | `PropGet` / `PropGetByNum` with `flag` (default `TJS_MEMBERMUSTEXIST`) |
| `Scripts.safeEvalStorage(name[, mode[, context]])` | static method | evaluates a `(const)[…]` storage and returns element `0` |
| `Scripts.rehash()` | static method | the VM's `TJSDoRehash` |
| `Scripts.pfMemberEnsure` | constant | `TJS_MEMBERENSURE` |
| `Scripts.pfMemberMustExist` | constant | `TJS_MEMBERMUSTEXIST` |
| `Scripts.pfIgnoreProp` | constant | `TJS_IGNOREPROP` |
| `Scripts.pfHiddenMember` | constant | `TJS_HIDDENMEMBER` |
| `Scripts.pfStaticMember` | constant | `TJS_STATICMEMBER` |

The reference attaches these with `NCB_ATTACH_CLASS` and most carry
`TJS_STATICMEMBER`, so the port exposes them as static members of `Scripts`
only — matching the reference. The core `Scripts` class also carries the
members from the `json` port; see [`json`](json.md).

`rehash` is bound to the VM's `TJSDoRehash`.

## Behaviour

### The object helpers

`getObjectKeys` returns the member names as a sorted TJS `Array`.
`getObjectCount` returns the member count; a `Dictionary` is counted by
enumeration. `getObjectContext` returns the object's context (`ObjThis`), and
`isNullContext` reports whether that context is null.

### Structural equality

`equalStruct` compares two values recursively, descending into arrays and
dictionaries and comparing the leaves with `===`. `equalStructNumericLoose`
is the same, except that an integer `0` and a real `0.0` compare equal.

### `foreach`

`foreach(obj, func, args*)` iterates the members of `obj` and calls
`func(memberValue, memberName, …args)` for each. A non-`void` return from the
callback stops the iteration and that value is returned; otherwise the result
is `void`.

The callback runs with the helper's calling context as its `this` when the
callback carries no receiver of its own
(`reference/cpp/plugins/scriptsEx.cpp:690-693`). The plain static dispatch
drops that receiver, so `foreach` is attached through the objthis dispatch
while still being a `TJS_STATICMEMBER`.

### `getMD5HashString`

The lowercase hex MD5 of the octet. The digest itself is the reference's
`TVP_md5_*` implementation, shared with the `fstat` plugin's
`Storages.getMD5HashString` (`crates/compat/src/natives/plugins/md5.rs`), so
the algorithm exists once instead of one hand-rolled copy per plugin.

### `clone`

`clone(obj)` makes a deep copy of an array or a dictionary. Any other object
is returned after the object's own `clone()` member has been invoked — the
reference's dispatch for a non-collection object.

### `propSet` / `propGet`

`propSet(obj, key, value[, flag])` performs `PropSet` (or `PropSetByNum` for
a numeric key) with the given flag; the default is `TJS_MEMBERENSURE`.

* With `TJS_MEMBERENSURE`, a missing member is created.
* Without it, the write fails on a missing member
  (`tTJSCustomObject::PropSet`).

`propGet(obj, key[, flag])` performs `PropGet` (or `PropGetByNum`); the
default is `TJS_MEMBERMUSTEXIST` (`reference/cpp/plugins/scriptsEx.cpp:700-714`).

* Under `TJS_MEMBERMUSTEXIST`, a missing member is an error. A `Dictionary`
  resolves an absent member to a cleared (void) result
  (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:732-742`).
* `TJS_MEMBERENSURE` *creates* the member as `void` before reading it.
* An out-of-range array index errors under `TJS_MEMBERMUSTEXIST` and reads as
  `void` otherwise.

### `safeEvalStorage`

`safeEvalStorage(name[, mode[, context]])` evaluates a `(const)[…]` storage
and returns element `0` of the result.

### `rehash`

`rehash()` calls the VM's `TJSDoRehash`, which forces the VM to rehash its
internal tables.

## State and lifetime

The plugin holds no per-object state: every member is static and reads its
arguments.

The recursive helpers operate on retained object ids and read or write
members through the engine's member API, so a script subclass's own override
wins, and no TJS source is generated from script data.

The `pf*` flags are plain class data members rather than accessor properties,
so a script assignment overwrites the value. The port writes them onto the
`Scripts` class object directly.

## Errors

| condition | outcome |
|---|---|
| `foreach` is called with fewer than two arguments | typed error: `Scripts.foreach requires at least 2 arguments` |
| `propGet` on a missing member under `TJS_MEMBERMUSTEXIST` | typed error |
| `propSet` on a missing member without `TJS_MEMBERENSURE` | typed error |
| `propGet` on an out-of-range array index under `TJS_MEMBERMUSTEXIST` | typed error |
| `safeEvalStorage` on a storage that cannot be read or evaluated | the storage's or the VM's error |
| a member is called with too few arguments | typed error naming the member |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/scripts_ex.rs` | the object helpers, both equality variants, `foreach` including its stopping value and its `this` receiver, the MD5 digest, `clone`, `propSet` / `propGet` under each flag, `safeEvalStorage`, `rehash`, and the `pf*` constants |

## Known gaps

* The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which
  the Rust ABI does not expose. The port reproduces that walk through the
  builtin `Array.assign`, which visits the same members in the same order.
* The reference's `Scripts` members are attached with `NCB_ATTACH_CLASS`,
  which the compat ABI has no exact equivalent for; the port attaches them as
  static members of `Scripts`, which is the shape the reference's
  `TJS_STATICMEMBER` flag produces anyway.

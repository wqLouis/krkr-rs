# ScriptsEx (`ScriptsEx.dll`)

**DLL name:** `ScriptsEx.dll` — linked with `Plugins.link("ScriptsEx.dll")`
**Rust module:** `crates/compat/src/natives/plugins/scripts_ex/`
**Reference:** `reference/cpp/plugins/scriptsEx.cpp`
**Registers:** static members on the core `Scripts` class

The plugin index is [`../plugins.md`](../plugins.md). The `Scripts` class also
carries the members of the [`json`](json.md) and [`window_ex`](window_ex.md)
ports.

## Surface

| member | kind | behaviour |
|---|---|---|
| `Scripts.getObjectKeys(obj)` | static method | the object's own member names, sorted, as a TJS `Array` |
| `Scripts.getObjectCount(obj)` | static method | the number of the object's own members |
| `Scripts.getObjectContext(obj)` | static method | the object's context (`ObjThis`), or TJS `null` |
| `Scripts.isNullContext(obj)` | static method | `1` when the object carries no context |
| `Scripts.equalStruct(a, b)` | static method | recursive structural equality |
| `Scripts.equalStructNumericLoose(a, b)` | static method | the same, with numeric leaves compared by value |
| `Scripts.foreach(obj, func[, extra...])` | static method | walks the elements of an array or the members of an object |
| `Scripts.getMD5HashString(octet)` | static method | the lowercase hex MD5 of the octet |
| `Scripts.clone(obj)` | static method | a deep copy of an array or a dictionary |
| `Scripts.propSet(obj, key, value[, flag])` | static method | writes a member under an explicit member flag |
| `Scripts.propGet(obj, key[, flag])` | static method | reads a member under an explicit member flag |
| `Scripts.safeEvalStorage(name[, mode[, context]])` | static method | evaluates a `(const)[…]` storage and returns element `0` |
| `Scripts.rehash()` | static method | the VM's own rehash entry point |
| `Scripts.pfMemberEnsure` | static data member | `0x00000200` — `TJS_MEMBERENSURE` |
| `Scripts.pfMemberMustExist` | static data member | `0x00000400` — `TJS_MEMBERMUSTEXIST` |
| `Scripts.pfIgnoreProp` | static data member | `0x00000800` — `TJS_IGNOREPROP` |
| `Scripts.pfHiddenMember` | static data member | `0x00001000` — `TJS_HIDDENMEMBER` |
| `Scripts.pfStaticMember` | static data member | `0x00010000` — `TJS_STATICMEMBER` |

The reference attaches the thirteen methods and the five flag values to the
`Scripts` class (`reference/cpp/plugins/scriptsEx.cpp:968-991`) and binds
`rehash` to the VM's `TJSDoRehash` (`:993`). The port registers them all as
static members of the `Scripts` class object, so a script reaches them
through `Scripts.<name>`.

The flag values are the VM's own member flags
(`reference/cpp/core/tjs2/tjsInterface.h:24-30`). They are plain class data
members, not accessor properties, so a script assignment overwrites the value
instead of raising.

## Behaviour

### Member names and counts

`getObjectKeys(obj)` returns the object's own member names as a TJS `Array`,
sorted. The reference builds the array with an `EnumMembers` walk and sorts it
with the array's own `sort` (`reference/cpp/plugins/scriptsEx.cpp:438-450`).

Two array cases follow the reference rather than the intuitive answer:

* An `Array` argument yields an **empty** array. The reference's
  `EnumMembers` walk is rejected by the array object with `TJS_E_NOTIMPL`
  (`crates/tjs/cpp/tjs2/tjsArray.h:221-224`), the callback never runs and the
  error is discarded, so the sorted array stays empty.
* `getObjectCount` on an `Array` yields `0`. The reference reads the dispatch
  `GetCount` (`reference/cpp/plugins/scriptsEx.cpp:468-483`), which the array
  object does not override (`crates/tjs/cpp/tjs2/tjsArray.h:212-215`), so the
  generic own-member count of `tTJSCustomObject::GetCount`
  (`crates/tjs/cpp/tjs2/tjsObject.cpp:1431-1444`) applies: an array keeps its
  elements in the native item vector, not in members.

Element access and the `count` *property* of an array are unaffected — they
still report the elements.

For every other object the count is the number of members the enumeration
reports.

The parity report records 24 reference members on `Scripts` and 33 registered
ones, of which 9 belong to the core `Scripts` class
([`docs/plugin_parity.md`](../../../plugin_parity.md)).

### Context

`getObjectContext(obj)` returns the object's context — the `ObjThis` of the
value's closure — and `isNullContext(obj)` reports whether that context is
missing (`reference/cpp/plugins/scriptsEx.cpp:488-502`). A dictionary carries
itself as its context; a bare anonymous function has none.

### Structural equality

`equalStruct(a, b)` descends into two arrays or two dictionaries and compares
their contents; every other pair of values is compared strictly, so an integer
and a real are never equal (`reference/cpp/plugins/scriptsEx.cpp:506-585`).

* Two arrays must have the same length, and every element pair is compared
  recursively.
* Two dictionaries must have the same key list, and every value pair is
  compared recursively.
* Two objects that are neither arrays nor dictionaries are compared by
  identity, which is what the reference's `DiscernCompare` does for two
  distinct objects.

`equalStructNumericLoose(a, b)` is the same walk, except that a numeric leaf
pair is compared by value, so `0` and `0.0` are equal
(`reference/cpp/plugins/scriptsEx.cpp:590-676`). Its dictionary branch first
requires the two dictionaries to have the same member count, then compares
every member the first dictionary holds; a key the second dictionary lacks is
skipped rather than failing the comparison.

### `foreach`

`foreach(obj, func[, extra...])` walks an array by index or the members of any
other object, calling `func(key, value, extra...)` for each step — the key is
the index for an array and the member name otherwise
(`reference/cpp/plugins/scriptsEx.cpp:681-749`). The extra arguments are
forwarded to the callback unchanged.

A callback that returns something other than `void` stops the walk, and that
value is what `foreach` returns; a walk that runs to the end returns `void`.

The callback runs with its own receiver when it has one, and with the
helper's calling context when it does not
(`reference/cpp/plugins/scriptsEx.cpp:690-693`) — the shape a KAG environment
relies on when it iterates a dictionary with an anonymous callback that reads
`this`. The port therefore attaches `foreach` through the objthis dispatch
while keeping it a static member of `Scripts`.

### `clone`

`clone(obj)` deep-copies an array or a dictionary
(`reference/cpp/plugins/scriptsEx.cpp:813-865`): a fresh array or dictionary is
built and every element or member is cloned recursively. Scalars and strings
come back unchanged.

Any other object is handled by the reference's fallback: the object's own
`clone` member is invoked, and the **original** object is returned. The
reference returns the clone's result only when the call reports `TJS_S_TRUE`,
while a successfully executed script function reports `TJS_S_OK` = `0`
(`reference/cpp/core/tjs2/tjsErrorDefs.h:32-36`) — so the comparison never
holds, the member's side effects run and the original object comes back. A
missing `clone` member and a non-callable `clone` member are error codes the
reference compares away as well; only a thrown script exception propagates.

### `propSet` and `propGet`

`propSet(obj, key, value[, flag])` writes a member and
`propGet(obj, key[, flag])` reads one. The flag defaults to
`TJS_MEMBERENSURE` for the write and to `TJS_MEMBERMUSTEXIST` for the read
(`reference/cpp/plugins/scriptsEx.cpp:869-905`); the `pf*` constants name the
same values.

An integer key addresses the member by number and any other key by name; on an
array both spellings reach the same element.

* A write to a missing member fails unless the flag carries
  `TJS_MEMBERENSURE` (`crates/tjs/cpp/tjs2/tjsObject.cpp:1352-1383`).
* A read of a missing member fails under `TJS_MEMBERMUSTEXIST`. A dictionary
  resolves an absent member to `void` when the flag does not carry it
  (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:738-749`).
* A read under `TJS_MEMBERENSURE` **creates** the member as `void` before
  reading it (`crates/tjs/cpp/tjs2/tjsObject.cpp:1303-1306`).
* An out-of-range array index fails under `TJS_MEMBERMUSTEXIST` and reads as
  `void` otherwise.

The existence probe behind those rules is a `typeof obj[key]` read, which
compiles to a member read under `TJS_MEMBERMUSTEXIST`
(`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1127-1130`): it reports
`"undefined"` for a missing member and the value's type name otherwise,
including `"void"` for a member that exists and holds void.

### `getMD5HashString`

`getMD5HashString(octet)` returns the MD5 digest of the octet as 32 lowercase
hex characters (`reference/cpp/plugins/scriptsEx.cpp:756-781`). The digest
itself is shared with the `fstat` port's `Storages.getMD5HashString`
(`crates/compat/src/natives/plugins/md5.rs`), so the algorithm exists once.

### `safeEvalStorage`

`safeEvalStorage(name[, mode[, context]])` reads a storage, wraps its content
in `(const)[…]`, evaluates it under the storage's base name and returns
element `0` of the resulting array
(`reference/cpp/plugins/scriptsEx.cpp:909-966`). The `(const)` wrapper is what
makes the read safe: the literal admits constants only, so a storage that is
not a constant raises instead of running.

The `mode` argument is honoured for its `oN` byte offset
(`reference/cpp/core/base/TextStream.cpp:75`). The `context` argument is the
execution context of the evaluated expression.

### `rehash`

`rehash()` calls the VM's `TJSDoRehash`, the entry point the reference binds
the member to (`reference/cpp/plugins/scriptsEx.cpp:993`). It returns `void`.

## State and lifetime

The plugin keeps no per-object state: every member is static and reads its
arguments.

The per-engine state lives in the compatibility services' dispatch state. It
holds a reusable byte buffer for the string and octet results the native
callback hands back, and the retained ids of the two helper closures the
recursive walks use — `(function(a,b){return a===b;})` for the identity
comparison and `(function(o,k){return typeof o[k];})` for the member-existence
probe. Both closures are compiled once per engine and kept on the engine, so a
deep `foreach` or `equalStruct` walk does not re-evaluate them.

The recursive helpers work on retained object ids and read or write members
through the engine's member API, so a script subclass's own override wins and
no TJS source is generated from script data.

## Errors

| condition | outcome |
|---|---|
| a member is called with fewer arguments than it needs | typed error naming the member and the count it needs |
| `propGet` on a missing member under `TJS_MEMBERMUSTEXIST` | typed error naming the member |
| `propSet` on a missing member without `TJS_MEMBERENSURE` | typed error naming the member |
| `propGet` or `propSet` on an out-of-range array index under `TJS_MEMBERMUSTEXIST` | typed error naming the member |
| `getMD5HashString` without an octet, or with a value that is not an octet | typed error |
| `safeEvalStorage` on a storage that cannot be read | the storage's error, prefixed with the member name |
| `safeEvalStorage` on a storage whose content is not a constant literal | the VM's syntax error |
| `safeEvalStorage` without an attached storage | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/scripts_ex.rs` | the sorted key list and the member count, the two array cases, the context helpers, both equality variants, `foreach` over arrays and dictionaries including its stopping value, its forwarded extra arguments and its `this` receiver, the deep copy of collections, the "invoke the member, return the original" fallback, the MD5 digest, `propSet` / `propGet` under each flag, the argument counts, `safeEvalStorage` on a constant literal and its rejection of a non-constant storage, `rehash`, and the `pf*` constants including their being overwritable |
| `crates/compat/tests/misc_plugins.rs` | that `Scripts.getObjectKeys` over a dictionary reports only the script's own keys, so the members other plugins attach do not leak into an enumeration |

## Known gaps

* The reference walks an object's members with `iTJSDispatch2::EnumMembers`,
  which the Rust ABI does not expose. The port reproduces that walk through
  the builtin `Array.assign`, whose non-array branch calls
  `EnumMembers(TJS_IGNOREPROP, …)` and appends `[key, value, …]`
  (`crates/tjs/cpp/tjs2/tjsArray.cpp:1084-1088`, `:1092-1117`). The same flag,
  the same member order and the same hidden-member filtering apply.
* `getObjectCount` counts the enumerated members, while the reference's
  `GetCount` counts every own member, hidden ones included
  (`crates/tjs/cpp/tjs2/tjsObject.cpp:1431-1444`). The two agree for an
  object without hidden members.
* `clone` of a dictionary copies the members in sorted key order and without
  the reference's per-member flags; the reference copies them in the
  enumeration order and carries the hidden and static bits over
  (`reference/cpp/plugins/scriptsEx.cpp:799-802`).
* `safeEvalStorage` decodes the storage into a Rust string, which drops an
  unpaired UTF-16 surrogate; the reference's text stream keeps every code unit
  (`crates/compat/src/text.rs:460-461`).

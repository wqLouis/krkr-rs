# saveStruct (`saveStruct.dll`)

**DLL name:** `saveStruct.dll` — linked with `Plugins.link("saveStruct.dll")`
**Rust module:** `crates/compat/src/natives/plugins/save_struct/`
**Reference:** `reference/cpp/plugins/saveStruct.cpp`
**Registers:** instance members on `Array`, static members on `Dictionary`

The plugin index is [`../plugins.md`](../plugins.md).

## Surface

| member | kind | behaviour |
|---|---|---|
| `Array.save2(filename[, utf[, newline]])` | instance method | writes every element's string form, one per line |
| `Array.saveStruct2(filename[, utf[, newline]])` | instance method | writes the `saveStruct` form of the array to a file |
| `Array.toStructString([newline])` | instance method | returns the `saveStruct` form of the array as a string |
| `Dictionary.saveStruct2(filename[, utf[, newline]])` | static method | writes the `saveStruct` form of the dictionary to a file |
| `Dictionary.toStructString([newline])` | static method | returns the `saveStruct` form of the dictionary as a string |

The reference attaches the three `Array` members without a static flag
(`reference/cpp/plugins/saveStruct.cpp:310-313`), so the creation callback
copies them onto every array instance
(`reference/cpp/core/tjs2/tjsNative.cpp:346-363`) and a script reaches them
through the array it operates on. The port registers them the same way, as
objthis-only instance members whose callbacks receive the array they were
invoked on.

The two `Dictionary` members carry `TJS_STATICMEMBER`
(`reference/cpp/plugins/saveStruct.cpp:364-366`), so they live on the
`Dictionary` class object — the flag sets the member's static bit
(`reference/cpp/core/tjs2/tjsObject.cpp:1390-1391`) — and the creation
callback skips them, so they are never copied onto an instance. The port
attaches them with `register_builtin_static_members`, which keeps that
placement: `Dictionary.saveStruct2` is reachable, `dict.saveStruct2` is not.

The `utf` argument is accepted and ignored, and the newline flag is read from
the third argument of the writers and from the first argument of
`toStructString` — the reference's own argument positions
(`reference/cpp/plugins/saveStruct.cpp:234-236`, `:279`, `:302`, `:339`,
`:355`).

The parity report counts 3 reference members on `Array` and 3 registered
ones, and 2 on `Dictionary` and 2 registered ones
([`docs/plugin_parity.md`](../../../plugin_parity.md)).

## Behaviour

### The serialized form

The serializer reproduces the reference's `getVariantString` /
`getArrayString` / `getDictString` (`reference/cpp/plugins/saveStruct.cpp:128-209`):

| TJS value | serialized form |
|---|---|
| `void` | `void` |
| TJS `null` | `nullptr` |
| integer | `int N` |
| real | `real N` |
| string | a double-quoted literal, with `"` and `\` escaped (`saveStruct.cpp:50-68`) |
| octet | `<% xx xx %>`, one lowercase hex byte and a trailing space each (`saveStruct.cpp:70-78`) |
| an `Array` instance | `[v0,v1,...]` — comma-separated, never a newline (`saveStruct.cpp:142-165`) |
| any other object | `%["key"=>value,...]` — entries separated by `,` and the configured newline (`saveStruct.cpp:128-137`, `:106-114`) |

An object is recognized as an array by its class chain, so a script subclass
of `Array` serializes as an array. Nested arrays and dictionaries recurse
through the same serializer.

The newline is CRLF when the flag is `0` and LF when it is any other value
(`reference/cpp/plugins/saveStruct.cpp:24-30`); it governs the dictionary
separator only, because array elements are always comma-joined.

No serialization crate applies: the `saveStruct` layout is the reference's own
text form — not JSON and not TOML (`int N`, `real N`, `%["k"=>v]`,
`<% xx %>`) — and the member enumeration has to run through the VM, so the
serializer stays a TJS script.

### Member enumeration

The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which the
Rust ABI does not expose. The serializer instead asks the builtin
`Array.assign` to copy the dictionary's own members into an array as
`[key, value, key, value, …]`. `Array.assign` runs exactly the reference's
walk: its non-array branch calls `EnumMembers(TJS_IGNOREPROP, …)` and its
callback appends the name and the value of every member that is not hidden
(`crates/tjs/cpp/tjs2/tjsArray.cpp:1084-1088`, `:1092-1117`). The member
order and the hidden-member filtering therefore match.

### The `save2` form

`save2` is the plain "one string per line" writer, not the `saveStruct`
writer: it writes each element's string form followed by the newline
(`reference/cpp/plugins/saveStruct.cpp:252-259`). The reference reads every
element through `tTJSVariant::GetString`, which accepts strings only, so a
non-string element is an error rather than a converted `"42"`.

### Files

`save2` and `saveStruct2` resolve their file name the way the reference's
`TVPCreateStream` does (`reference/cpp/plugins/saveStruct.cpp:232-233`):

* a relative name resolves below the attached storage's game directory, with
  every existing path component replaced by its real on-disk spelling;
* an absolute name below the game directory resolves the same way;
* an absolute name outside the game directory is used as given;
* a Windows drive-letter path (`C:/…`) counts as absolute;
* with no storage attached, the name is used as given.

The parent directories are created before the file is written.

The bytes are the raw `tjs_char` stream the reference's `tTVPStringStream`
emits (`reference/cpp/plugins/saveStruct.cpp:32-48`): UTF-16LE with no BOM
and no crypt container.

## State and lifetime

The serializer lives in a TJS namespace object that the module installs once
per engine and then removes from the global object. The reference exposes no
free functions (`reference/cpp/plugins/saveStruct.cpp:310-314`, `:364-367`),
so nothing of the helper stays reachable as a global.

The namespace is retained by the engine, and its retained id is kept in the
per-engine dispatch state (`SaveStructState::namespace`). The id is
deliberately not released on the Rust side: the engine's retained map dies
with the VM, and the VM frees the C++ retention.

The serializer holds no other state between calls.

## Errors

| condition | outcome |
|---|---|
| `save2` or `saveStruct2` without a file name | typed error: `<member> requires 1 argument` |
| `save2` with a non-string element | typed error naming the element index |
| the serializer is not installed | typed error |
| the file cannot be written | the file system's error, prefixed with the path |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/save_struct.rs` | the serialized form of every value type, the string quoting and escaping, the octet form, `save2`'s line-per-element output and its newline selection, `saveStruct2`'s output, the relative-path resolution, the non-string element error, the dictionary forms and their separator, the newline flag for the dictionary separator, the nested structures, the empty array and the empty dictionary, the member placement on the class and on the instances, the absence of the helper globals, the argument counts, and the ignored `utf` flag |
| `crates/compat/tests/misc_plugins.rs` | that the `Dictionary` members are reachable only on the class object, and that a plain dictionary serializes only its own script keys |

## Known gaps

* The reference's dictionary walk uses `EnumMembers`; the port reproduces it
  through the builtin `Array.assign`, which runs the same walk with the same
  flag and applies the same hidden-member filtering.
* The reference's file writers document a `utf` argument that selects a UTF-8
  output, but the code that would honour it is commented out
  (`reference/cpp/plugins/saveStruct.cpp:234-236`), so the reference writes
  the 16-bit `tjs_char` stream. The port matches the reference and writes the
  same UTF-16LE bytes; the documented UTF-8 output does not exist in either.
* The reference opens its write target through the storage manager, whose
  name grammar can address an archive member. The port resolves the name
  through the attached storage and writes to the host filesystem, so the file
  lands below the game directory.
* The reference's `Dictionary.toStructString` writes a terminating NUL into
  its buffer (`reference/cpp/plugins/saveStruct.cpp:357`); the port returns a
  TJS string, which carries no terminator.

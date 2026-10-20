# saveStruct (`saveStruct.dll`)

**DLL name:** `saveStruct.dll` — linked with `Plugins.link("saveStruct.dll")`
**Rust module:** `crates/compat/src/natives/plugins/save_struct/`
**Reference:** `reference/cpp/plugins/saveStruct.cpp`
**Registers:** instance members on `Array` and static members on `Dictionary`

## Surface

| member | kind | behaviour |
|---|---|---|
| `Array.save2(name)` | instance method | writes every element's string form plus a newline |
| `Array.saveStruct2(name)` | instance method | writes the `saveStruct` form to a file |
| `Array.toStructString()` | instance method | returns the `saveStruct` form as a string |
| `Dictionary.saveStruct2(name)` | static method | writes the `saveStruct` form to a file |
| `Dictionary.toStructString()` | static method | returns the `saveStruct` form as a string |

The reference attaches these with `NCB_ATTACH_CLASS`. Its `ArrayAdd` members
use no static flag (`reference/cpp/plugins/saveStruct.cpp:310-313`), so the
port registers them as objthis-only instance members and the callbacks
receive the array they were invoked on.

Its `DictAdd` members carry `TJS_STATICMEMBER`
(`reference/cpp/plugins/saveStruct.cpp:364-366`), so the port attaches them
with `register_builtin_static_members`: they live on the `Dictionary` class
object and are not copied onto instances.

## Behaviour

### The serialized form

The serializer reproduces the reference's `getVariantString` /
`getArrayString` / `getDictString` exactly:

| TJS value | serialized form |
|---|---|
| `void` | `void` |
| TJS `null` | `nullptr` |
| integer | `int N` |
| real | `real N` |
| string | a double-quoted literal, with `"` and `\` escaped |
| octet | `<% xx xx %>` |
| an `Array` instance | `[v0,v1,...]` — comma-separated, no newline |
| any other object | `%["key"=>value,...]` — entries separated by `,` and the configured newline (CRLF unless the newline flag is non-zero) |

An `Array` instance is recognized by its class chain, so a script subclass of
`Array` is serialized as an array.

### Member enumeration

The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which the
Rust ABI does not expose. The serializer instead asks the builtin
`Array.assign` to copy the dictionary's non-hidden members into an array as
`[key, value, key, value, ...]`; `Array.assign` performs exactly the
reference's `EnumMembers` walk, so the member order and hidden-member
filtering match. Nested objects recurse through the same script.

No serialization crate applies: the `saveStruct` format is the reference's own
text layout — not JSON and not TOML (`int N`, `real N`, `%["k"=>v]`,
`<% xx %>`) — and member enumeration must run through the VM
(`Array.assign`), so the serializer stays the reference's TJS script.

### The `Array.save2` form

`save2` writes every element's **string form** plus a newline — the plain
string of each element, one per line, not the `saveStruct` form.

### Files

`save2` and `saveStruct2` resolve a relative name against the attached
storage's game directory; an absolute name is used as-is. They write the raw
`tjs_char` stream the reference's `tTVPStringStream` produces: UTF-16LE with
no BOM and no crypt container.

## State and lifetime

The serializer lives in a TJS namespace object that the module installs once
per engine and then removes from the global object, keeping it alive in the
per-engine dispatch state (`SaveStructState::namespace`). The reference
exposes no free functions (`reference/cpp/plugins/saveStruct.cpp:310-314`,
`:364-367`), so the namespace must not stay on the global object.

The serializer itself holds no state between calls beyond the namespace.

## Errors

| condition | outcome |
|---|---|
| `save2` / `saveStruct2` without a file name | typed error |
| the storage is not attached | typed error |
| the file cannot be written | the storage's error |
| `toStructString` on a value that cannot be serialized | the VM's error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/save_struct.rs` | the serialized form of every value type, the dictionary enumeration order and hidden-member filtering, `save2`'s line-per-element output, the file encoding (UTF-16LE, no BOM), the relative-path resolution, and the `Array` / `Dictionary` member placement |
| `crates/compat/tests/misc_plugins.rs` | that the `Dictionary` members are reachable like the reference's static ones, and that the static plugin members do not confuse the parameter macros |

## Known gaps

* The reference's dictionary walk uses `EnumMembers`; the port reproduces it
  through the builtin `Array.assign`, which visits the same members in the
  same order and applies the same hidden-member filtering.
* The reference writes through `tTVPStringStream`, whose output is a
  `tjs_char` (UTF-16LE) stream. The port writes the same bytes, but the file
  it produces is not wrapped in the reference's crypt container — the
  reference does not use one either.

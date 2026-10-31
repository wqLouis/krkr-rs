# simplebinder (plugin-authoring helper)

**DLL name:** none — `simplebinder` is not a plugin DLL
**Rust module:** `crates/compat/src/natives/plugins/simple_binder/`
**Reference:** `reference/cpp/plugins/simplebinder/` (`simplebinder.hpp`,
`simplebinder.txt`, `readme.txt`)
**Registers:** nothing

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the helper has no script-visible class or function |

`simplebinder` is **not a plugin**. The reference directory contains only
`simplebinder.hpp`, `simplebinder.txt` and `readme.txt` — a header-only C++
utility library that other plugins `#include` to make their TJS registration
terser. `simplebinder.hpp` defines no `NCB_*` macro use, no
`TJS_BEGIN_NATIVE_MEMBERS` block and no `TJSCreateNativeClassForPlugin` call;
it is `CMakeLists.txt`-listed as an include directory
(`target_include_directories(... ${KRKR2PLUGIN_PATH}/simplebinder)`) and never
compiled into the plugin static library. The only in-tree consumer is the
optional, commented-out `DrawDeviceForSteam` plugin.

## Behaviour

The helper exists so a plugin can attach C++ callbacks to an
`iTJSDispatch2` *store* (the global object or an existing class object) at
link time:

* `Function` / `Property` / `Constant` / `Variant` create the reference's
  `tTJSDispatch`-derived stores (`FunctionStore`, `PropertyStore`,
  `ConstantStore`) and `PropSet` them onto the store;
* `Class` builds a native class through `TJSCreateNativeClassForPlugin`;
* `GetObject` walks a dot-separated object path.

The Rust binding surface already provides the equivalents declaratively:
`CompatCtx::register_class`, `CompatCtx::register_instance`,
`CompatCtx::register_static_members` and `CompatCtx::register_instance_members`.
A Rust port of the C++ header would be a parallel wrapper over those builders
that hides no new behaviour, so the module registers nothing.

There is no `Plugins.link` name for this helper, because the reference ships
no DLL for it: it is not an entry in the built-in table, so a script that
links `simplebinder.dll` finds it absent, as the
[plugin index](../plugins.md) describes.

## State and lifetime

No state: the module exposes no items at all — there is no registration
function to call.

## Errors

None. The module has no code path that can fail.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | that the helper-only globals (`SimpleBinder`, `Wutcwf`, `TCWFDecoder`) are **not** registered |

## Known gaps

* None. The reference helper registers no script-visible surface, so the port
  has none to install. This is the same situation as
  [`emote_player`](emote_player.md), a documented no-op module that keeps the
  plugin-name/module mapping aligned with the reference DLL set.

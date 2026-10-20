# getabout (`getabout.dll`)

**DLL name:** `getabout.dll` — linked with `Plugins.link("getabout.dll")`
**Rust module:** `crates/compat/src/natives/plugins/get_about/`
**Reference:** `reference/cpp/plugins/getabout.cpp`
**Registers:** nothing — the member it owns is registered by the `windowEx` port
**Depends on:** `windowEx.dll`

## Surface

| member | kind | behaviour |
|---|---|---|
| `System.getAboutString` | static method | registered by the [`windowEx`](window_ex.md) port, not by this module |

The reference plugin is a single attachment —
`NCB_ATTACH_FUNCTION(getAboutString, System, TVPGetAboutString)`
(`reference/cpp/plugins/getabout.cpp`) — so it adds one member to the
existing `System` class and defines no class of its own.

The compat VM rejects a duplicate class member, so the member cannot be
registered twice. The `windowEx` port owns the whole `System` extension
surface and registers `System.getAboutString` when the **`windowEx` surface is
linked** (`crates/compat/src/natives/plugins/window_ex/system.rs`). This module
therefore installs nothing.

## Behaviour

`Plugins.link("getabout.dll")` resolves to the built-in table and succeeds.
The entry declares `windowEx.dll` as a **dependency**, so the link registers
the `windowEx` surface first and `System.getAboutString` is on the VM by the
time the link returns.

The dependency is **not** a link of its own: `Plugins.getList` reports
`getabout.dll` alone, never `windowEx.dll` — the dependency is the reference's
pre-registration load, not something the script asked for.

A game that links `getabout.dll` and then reads `System.getAboutString` sees
the same value it would on the reference.

## State and lifetime

No state: the module registers nothing.

## Errors

None. `register` always succeeds; the plugin's only failure mode in the
reference — a duplicate `System.getAboutString` — cannot occur.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/get_about.rs` | that `System.getAboutString` is attached to the `System` class |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("getabout.dll")` resolves to the built-in table |

## Known gaps

* None. The surface is complete; only the *ownership* of the member differs
  from the reference, and the ownership is invisible to a script.

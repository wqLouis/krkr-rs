# getabout (`getabout.dll`)

**DLL name:** `getabout.dll` (aliases: none) — linked with `Plugins.link("getabout.dll")`
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
(`reference/cpp/plugins/getabout.cpp:5`) — so it adds one member to the
*existing* `System` class and defines no class of its own.

The compat VM rejects a duplicate class member, so the member cannot be
registered twice. The `windowEx` port owns the whole `System` extension
surface and registers `System.getAboutString` in
`crates/compat/src/natives/plugins/window_ex/system.rs`. This entry declares
`windowEx.dll` as a **dependency**
(`crates/compat/src/natives/plugins/mod.rs`), so linking `getabout.dll` brings
the `windowEx` surface in with it. The module itself installs nothing.

## Behaviour

`Plugins.link("getabout.dll")` resolves to the built-in table and succeeds.
The registry registers the dependency first, so `System.getAboutString` is on
the VM by the time the link returns.

The dependency is **not** a link of its own: `Plugins.getList` reports
`getabout.dll` alone, never `windowEx.dll` — the dependency is the reference's
pre-registration load, not something the script asked for.

`System.getAboutString()` returns the about string. The reference returns
`TVPGetAboutString` (`reference/cpp/core/base/MsgIntf.cpp:144`); the port
returns its own host string. The member is reachable through `System` only,
never as a free global.

## State and lifetime

No state: the module registers nothing.

## Errors

None. `register` always succeeds; the reference's only failure mode — a
duplicate `System.getAboutString` — cannot occur.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/get_about.rs` | that `System.getAboutString` is attached to `System`, returns a non-empty string, and is not reachable as a free global |
| `crates/compat/tests/window_ex.rs` | that `System.getAboutString()` returns a non-empty string among the `windowEx` `System` literals |
| `crates/compat/tests/dispatch_fmt.rs` | that the no-op plugins (`GetAbout`, `VarFile`, `Extrans`) publish no global of their own |

## Known gaps

* The about string's *text* differs: the reference returns the engine's
  `TVPGetAboutString` text, the port returns its own host string. The member,
  its kind and the fact that it returns a non-empty string are the same.
* The ownership of the member differs — the `windowEx` port registers it and
  `getabout.dll` only declares the dependency — and the ownership is invisible
  to a script.

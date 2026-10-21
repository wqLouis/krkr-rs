# emoteplayer (`emoteplayer.dll`)

**DLL name:** `emoteplayer.dll` (aliases: none) — linked with `Plugins.link("emoteplayer.dll")`
**Rust module:** `crates/compat/src/natives/plugins/emote_player/`
**Reference:** `reference/cpp/plugins/EmotePlayer.cpp`
**Registers:** nothing — the loader shim for `motionplayer.dll`
**Depends on:** `motionplayer.dll`

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The entire reference module is a pre-registration callback that loads
`motionplayer.dll` as a dependency:

```cpp
#define NCB_MODULE_NAME TJS_W("emoteplayer.dll")

static void EmotePlayerPreRegist() {
    // Load motionplayer.dll as dependency
    ncbAutoRegister::LoadModule(TJS_W("motionplayer.dll"));
}

NCB_PRE_REGIST_CALLBACK(EmotePlayerPreRegist);
```

(`reference/cpp/plugins/EmotePlayer.cpp:5-8`.)

`emoteplayer.dll` registers **no** classes of its own. The `Motion` namespace
belongs to `motionplayer.dll`: the `Motion` class with the `Motion.Player`,
`Motion.EmotePlayer`, `Motion.ResourceManager`, `Motion.SeparateLayerAdaptor`,
`Motion.D3DAdaptor`, `Motion.SourceCache`, `Motion.ObjSource`, `Motion.Point`,
`Motion.Circle`, `Motion.Rect`, `Motion.Quad` and `Motion.LayerGetter`
subclasses, plus the top-level `Player`, `D3DEmotePlayer` and
`D3DEmoteModule` classes
(`reference/cpp/plugins/motionplayer/main.cpp:128,427,565,588`).

## Behaviour

`Plugins.link("emoteplayer.dll")` resolves to the built-in table and succeeds.
The table declares `motionplayer.dll` as this entry's **dependency**
(`crates/compat/src/natives/plugins/mod.rs`), so the registry registers the
`motion_player` port first: the `Motion` namespace and its classes are on the
VM by the time the link returns, exactly as the reference's loader shim pulls
the module in before its own registration.

The dependency is **not** a link of its own: `Plugins.getList` reports
`emoteplayer.dll` alone, never `motionplayer.dll`.

The classes exist only where the reference puts them. The reference's
`NCB_REGISTER_SUBCLASS_DELAY` defines members without publishing a global
(`reference/cpp/core/plugin/ncbind.hpp:1963-1970`) and `NCB_SUBCLASS` attaches
the class under `Motion`
(`reference/cpp/plugins/motionplayer/main.cpp:427-441`), so only `Player`,
`Motion`, `D3DEmotePlayer` and `D3DEmoteModule` are top-level. The port
registers each class through the binding API, aliases it into `Motion` and then
deletes the registration global, so the namespace matches the reference.

## State and lifetime

No state: the module registers nothing. The `Motion` classes and the state
they own belong to the `motion_player` port
([`motion_player`](motion_player.md)).

## Errors

None. `register` always succeeds.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/emote_player.rs` | that the loader defines no global of its own; that the link is idempotent, installs no null plugin object, and brings in the `Motion` namespace with its classes and the top-level `D3DEmotePlayer` / `D3DEmoteModule` |
| `crates/compat/tests/motion_player.rs` | that the namespace-only classes are **not** published as top-level globals, stay reachable under `Motion`, and that `Player`, `D3DEmotePlayer` and `D3DEmoteModule` stay top-level |

## Known gaps

* None in the loader itself. The reference splits the classes across two DLLs;
  the port registers them from the `motion_player` port and aliases them into
  `Motion`, so the observable namespace — and the set of top-level classes —
  matches the reference.

# emoteplayer (`emoteplayer.dll`)

**DLL name:** `emoteplayer.dll` — linked with `Plugins.link("emoteplayer.dll")`
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

(`reference/cpp/plugins/EmotePlayer.cpp`.)

`emoteplayer.dll` registers **no** classes of its own. Its only job is to
load `motionplayer.dll`; that DLL owns the whole `Motion` namespace
(`Motion`, `Player`, `EmotePlayer`, `D3DEmotePlayer`, `D3DEmoteModule`,
`ResourceManager`, `SeparateLayerAdaptor`, `D3DAdaptor`, `SourceCache`,
`ObjSource`, `Point`, `Circle`, `Rect`, `Quad`, `LayerGetter`).

## Behaviour

`Plugins.link("emoteplayer.dll")` resolves to the built-in table and succeeds.
The table declares `motionplayer.dll` as this entry's **dependency**, so the
registry registers the `motion_player` port first: the `Motion` namespace and
its classes are on the VM by the time the link returns, exactly as the
reference's loader shim pulls the module in before its own registration.

The dependency is **not** a link of its own: `Plugins.getList` reports
`emoteplayer.dll` alone, never `motionplayer.dll`.

The port publishes every `Motion.*` subclass top-level (the class-registration
API has no namespace-only form) and then aliases it into `Motion`. The extra
top-level names are accepted and locked in
`docs/surface/runtime_surface.json`.

## State and lifetime

No state: the module registers nothing.

## Errors

None. `register` always succeeds.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/emote_player.rs` | the loader defines no globals of its own; the loader exposes the `motionplayer` classes |
| `crates/compat/tests/motion_player.rs` | that the `Motion` classes are registered exactly once and that a second install is idempotent |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("emoteplayer.dll")` resolves to the built-in table |

## Known gaps

* The reference exposes no top-level `EmotePlayer`; the class is a `Motion.*`
  subclass (`reference/cpp/plugins/motionplayer/main.cpp:27-124`). The port
  publishes every `Motion.*` subclass top-level and then aliases it into
  `Motion`, so the top-level names exist as well. The extra names are locked
  into the surface baseline.
* The reference's dependency load happens at *link* time; the port does the
  same — the `motionplayer.dll` classes appear when the script links
  `emoteplayer.dll`. A script that links `emoteplayer.dll` first and then
  `motionplayer.dll` sees the same classes in both orders, because the
  registry registers a module once.

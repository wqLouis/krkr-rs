# Native plugins (`compat`)

The native plugins are the emulations of the KiriKiri plugin DLLs a game
names in `Plugins.link("…dll")`. They live in the compatibility layer
(`crates/compat`), are **statically built into the binary**, and are the only
place where a plugin's script-visible surface is registered.

* Source: `crates/compat/src/natives/plugins/`
* Registry: `crates/compat/src/plugin.rs` (`PluginRegistry`, `BuiltinPluginDef`)
* Script-facing class: `crates/compat/src/plugins_native.rs` (`Plugins`)
* Parity report: [`docs/plugin_parity.md`](../../plugin_parity.md)
* Design background: [`docs.old/migration/08-tjs-vm-and-native-plugins.md`](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md)

## How a plugin is linked

`Plugins` exposes three static members:

| member | returns | behaviour |
|---|---|---|
| `Plugins.link(name)` | `void` | activates the built-in emulation for `name`, or records the link and warns when the name is not ported |
| `Plugins.unlink(name)` | `bool` | forgets a linked name; returns whether it had been linked |
| `Plugins.getList()` | `Array` | the linked names, in insertion order and with the script's original casing |

`link` never throws for an unknown DLL. A name that resolves to a ported
plugin resolves to the built-in table; any other name is **absent**: the link
is recorded, one warning names the plugin, and nothing is registered for it
— no script global under the DLL's stem and no object that accepts every
member read and every call. A game that probes for the class
(`typeof global[name] == 'Object'`) therefore sees it missing and takes its
own "class not found" path, exactly as it does against an engine that could
not load the DLL.

A `.tpm` file in the executable directory, `<exe>/system` or `<exe>/plugin` is
auto-loaded at startup: the trailing `.tpm` becomes `.dll` and the name is
linked the same way, so `extrans.tpm` resolves to the same built-in as
`Plugins.link("extrans.dll")`.

Only the startup module (`xp3filter.dll`) is registered before the startup
scripts run; every other built-in registers its members on the first
`Plugins.link` that names it, the same point at which the reference runs a
module's `V2Link` registration. The timing is script-observable: a shipped
`system/voice.tjs` probes `typeof WaveSoundBuffer.setDefaultAheads` before it
links `getSample.dll` and only then publishes its own
`WaveSoundBuffer.enableGetSample` flag. Registration is idempotent: linking a
built-in whose classes are already installed records the link without
registering again.

A plugin may declare a startup dependency on another module. `emoteplayer.dll`
loads `motionplayer.dll` first, and `getabout.dll` loads `windowEx.dll`
because the `System.getAboutString` member lives in that module's extension;
the dependency is registered on the same link but is not reported by
`Plugins.getList`, exactly like the reference
pre-registration callback.

## The built-in table

`crates/compat/src/natives/plugins/mod.rs` holds one `BuiltinPluginDef` per
DLL name. The table is also the source of the parity report, which compares
the reference plugin surface (`reference/cpp/plugins/**`) against the classes
the port registers.

| DLL name | aliases | Rust module | classes / globals it installs | page |
|---|---|---|---|---|
| `addFont.dll` | — | `add_font` | `System.addFont` (static) | [add_font](plugins/add_font.md) |
| `csvParser.dll` | — | `csv_parser` | `CSVParser` | [csv_parser](plugins/csv_parser.md) |
| `dirlist.dll` | — | `dirlist` | global `getDirList` | [dirlist](plugins/dirlist.md) |
| `emoteplayer.dll` | — | `emote_player` | none (loader shim for `motionplayer.dll`) | [emote_player](plugins/emote_player.md) |
| `extrans.dll` | — | `extrans` | none (the transition providers are dispatched by `Layer.beginTransition`) | [extrans](plugins/extrans.md) |
| `fftgraph.dll` | — | `fft_graph` | global `drawFFTGraph` | [fft_graph](plugins/fft_graph.md) |
| `fstat.dll` | — | `fstat` | `Storages.*` stat members, `TemporaryFiles` | [fstat](plugins/fstat.md) |
| `getabout.dll` | — | `get_about` | none (`System.getAboutString` is owned by `windowEx`) | [get_about](plugins/get_about.md) |
| `getSample.dll` | — | `get_sample` | members on `WaveSoundBuffer` (`getSample`, `sampleValue`, `sampleCount`, `sampleAhead`, the defaults setters) | [get_sample](plugins/get_sample.md) |
| `json.dll` | — | `json` | `Scripts.evalJSON`, `evalJSONStorage`, `saveJSON`, `toJSONString` | [json](plugins/json.md) |
| `KAGParserEx.dll` | `KAGParser.dll` | `natives::kag` (core) | none — `KAGParser` is a core native registered at startup | — (core native, not a plugin module) |
| `layerExDraw.dll` | `layerExMovie.dll`, `perspective.dll` | `layer_ex` | the `GdiPlus` namespace, `Layer.draw*`, `Layer.perspectiveCopy`, the movie members | [layer_ex](plugins/layer_ex.md) |
| `menu.dll` | — | `natives::menu_item` | `MenuItem` | — (core native, not a plugin module) |
| `motionplayer.dll` | — | `motion_player` | `Motion`, `Player`, the `Motion.*` subclasses, the emote classes | [motion_player](plugins/motion_player.md) |
| `psbfile.dll` | — | `psb_file` | `PSBFile` | [psb_file](plugins/psb_file.md) |
| `psd.dll` | — | `psd_file` | `PSD` | [psd_file](plugins/psd_file.md) |
| `saveStruct.dll` | — | `save_struct` | `Array.save2`/`saveStruct2`/`toStructString`, `Dictionary.saveStruct2`/`toStructString` | [save_struct](plugins/save_struct.md) |
| `ScriptsEx.dll` | — | `scripts_ex` | the `Scripts.*` helpers, `Scripts.rehash` | [scripts_ex](plugins/scripts_ex.md) |
| `krkrsteam.dll` | `steam_api.dll`, `DrawDeviceForSteam.dll` | `steam` | `Steam`, `steam_api`, `DrawDeviceForSteam` | [steam](plugins/steam.md) |
| `TextRender.dll` | — | `text_render` | `TextRenderBase` | [text_render](plugins/text_render.md) |
| `varfile.dll` | — | `var_file` | none (the `var://` media lives in the storage layer) | [var_file](plugins/var_file.md) |
| `win32dialog.dll` | — | `win32_dialog` | `WIN32Dialog` | [win32_dialog](plugins/win32_dialog.md) |
| `windowEx.dll` | — | `window_ex` | members on `System`, `Scripts`, `Window`, `MenuItem`, `Console` and `Pad` | [window_ex](plugins/window_ex.md) |
| `wutcwf.dll` | — | `wutcwf` | none (it installs a `TCWF` wave decoder into the audio chain) | [wutcwf](plugins/wutcwf.md) |
| `wuvorbis.dll` | — | `wuvorbis` | none (the decoder lives in the audio chain) | [wuvorbis](plugins/wuvorbis.md) |
| `xp3filter.dll` | — | `xp3_filter` | `Storages.setXP3ArchiveExtractionFilter`, `Storages.setXP3ArchiveContentFilter` (deferred half) | [xp3_filter](plugins/xp3_filter.md) |

`simplebinder` is not a plugin DLL: the reference ships it as a header-only
C++ helper other plugins include, so the Rust module
(`crates/compat/src/natives/plugins/simple_binder/`) installs nothing. Its page
is [simple_binder](plugins/simple_binder.md).

## Conventions used by the plugin modules

* **A plugin is thin.** The module marshals arguments into an engine
  operation and marshals the result back. Pixel work, layout, audio mixing and
  container parsing belong to the engine and platform crates; a plugin module
  keeps only the dispatch state that has no home there.
* **State is per engine, never process-global.** Per-object payloads live in
  the native instance; per-engine state lives in the compatibility services'
  dispatch state.
* **Errors are typed.** A member that cannot do what the reference does raises
  the reference's error rather than returning a substitute value. The null
  plugin object is the only exception, and it warns on every call.
* **The reference is the oracle.** Every behaviour a script can observe
  matches `reference/cpp/**`; a knowingly-left divergence is recorded in the
  plugin's own page under *Known gaps*.
* **The surface is locked.** `krkr-gate surface-lock` fails when a registered
  member is added or removed, and `krkr-gate plugin-parity` fails when a
  reference member goes missing. Both are part of the gate.

## Reading order for a plugin page

Each page has the same sections:

1. **Surface** — the DLL name(s), the class or global it installs, and the
   registered member list.
2. **Behaviour** — what the members do, in the terms a script sees.
3. **State and lifetime** — where the per-object and per-engine state lives
   and when it is released.
4. **Errors** — what raises, and with which message.
5. **Tests** — the test files that pin the behaviour.
6. **Known gaps** — the disclosed divergences from the reference.

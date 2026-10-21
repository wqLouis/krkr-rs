# extrans (`extrans.dll`)

**DLL name:** `extrans.dll` (aliases: none) — linked with `Plugins.link("extrans.dll")`
**Rust module:** `crates/compat/src/natives/plugins/extrans/`
**Reference:** `reference/cpp/plugins/{wave,mosaic,turn,rotatetrans,ripple}.cpp`
**Registers:** nothing — the transition providers are dispatched by
`Layer.beginTransition`

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The reference plugin's `V2Link` registers transition **handler providers** into
the engine's global registry; `Layer.beginTransition` then looks them up by
name. The port dispatches the same names from
`crates/compat/src/natives/visual/layer/transitions.rs`, with the transition
primitives in the engine's `trans` module (`crates/engine/src/trans/mod.rs`),
so this module registers no TJS classes.

## Behaviour

`Plugins.link("extrans.dll")` resolves to the built-in table and succeeds.
The link itself changes nothing about the transition surface: the providers are
dispatched by `Layer.beginTransition`, whether or not the DLL was linked.

The provider names the reference plugin contributes are `wave`
(`reference/cpp/plugins/wave.cpp:297`), `mosaic`
(`reference/cpp/plugins/mosaic.cpp:408`), `turn`
(`reference/cpp/plugins/turn.cpp:526`), `rotatezoom`, `rotatevanish` and
`rotateswap` (`reference/cpp/plugins/rotatetrans.cpp:152,252,424`) and
`ripple` (`reference/cpp/plugins/ripple.cpp:1549`), alongside the engine's
built-in `crossfade`, `universal` and `scroll`
(`reference/cpp/core/visual/TransIntf.cpp:1237-1253`). The port dispatches the
same set:

| name | reference source | behaviour |
|---|---|---|
| `crossfade` | engine built-in | the default cross-fade |
| `universal` | engine built-in | the universal rule-image transition |
| `scroll` | engine built-in | a directional scroll |
| `wave` | `wave.cpp` | the wave transition |
| `mosaic` | `mosaic.cpp` | the mosaic transition |
| `turn` | `turn.cpp` | the turn transition |
| `rotatezoom` | `rotatetrans.cpp` | rotate + zoom |
| `rotatevanish` | `rotatetrans.cpp` | rotate + vanish |
| `rotateswap` | `rotatetrans.cpp` | rotate + swap |
| `ripple` | `ripple.cpp` | the ripple transition |

The port additionally accepts the spellings `""` (the KAG default method),
`fade`, `slide` and `push` for the cross-fade and the scroll, plus a `mask`
name for the universal rule-image transition with a hard threshold
(`crates/engine/src/trans/mod.rs`).

Every option is read through the shared option helpers and validated inside the
engine, so an out-of-range value is a typed error rather than a silent clamp.
The defaults are the reference's (`wave.cpp:327-331`, `mosaic.cpp:436-445`,
`turn.cpp:555-563`, `rotatetrans.cpp:183-193,284-291,453-465`,
`ripple.cpp:1574-1627`).

## State and lifetime

No state: the module registers nothing. The transition itself lives in the
engine's `Trans` value (`crates/engine/src/trans/mod.rs`), which the layer
transition state owns for the duration of the transition.

## Errors

None from this module. A transition name the dispatcher does not know, or an
option outside its range, is reported by the transition dispatcher as a typed
error during `Layer.beginTransition`.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/transitions.rs` | each provider's output, the reference option defaults, that the options reach the handlers, and that an invalid option is a typed error |
| `crates/compat/tests/layer.rs` | that every `extrans` provider name starts and stops a real transition, and that an unknown name or an invalid option raises a typed error |
| `crates/compat/tests/plugin.rs` | that `extrans.dll` — one of the DLLs the shipped game links — resolves to the built-in table |
| `crates/compat/tests/null_plugin.rs` | that linking `extrans.dll` installs no null plugin object under the DLL's stem |

## Known gaps

* The reference registers its providers into a **global** registry that a
  script could, in principle, extend. The port's dispatcher is a fixed match,
  so a script cannot add a transition provider of its own. No reference plugin
  exposes that registry to TJS, so nothing script-visible changes.
* An **unknown** transition name is not an error in the reference: the
  provider lookup shows a warning box and falls back to `crossfade`
  (`reference/cpp/core/visual/TransIntf.cpp:332-341`). The port raises a typed
  error instead (`Layer.beginTransition: unsupported transition …`). The
  port's extra spellings (`""`, `fade`, `slide`, `push`, `mask`) are its own;
  the reference knows none of them, so in the reference they would all take the
  warning-and-cross-fade path.

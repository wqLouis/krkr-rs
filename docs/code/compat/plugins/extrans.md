# extrans (`extrans.dll`)

**DLL name:** `extrans.dll` — linked with `Plugins.link("extrans.dll")`
**Rust module:** `crates/compat/src/natives/plugins/extrans/`
**Reference:** `reference/cpp/plugins/{wave,mosaic,turn,rotatetrans,ripple}.cpp`
**Registers:** nothing — the transition providers are dispatched by `Layer.beginTransition`

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The reference plugin's `V2Link` registers transition **handler providers**
into the engine's global registry; `Layer.beginTransition` then looks them up
by name. The port dispatches the same names from
`crates/compat/src/natives/visual/layer/transitions.rs`, with the transition
primitives in the engine's `trans` module, so this module registers no TJS
classes.

## Behaviour

`Plugins.link("extrans.dll")` resolves to the built-in table and succeeds.

The provider names the reference plugin contributes are `wave`, `mosaic`,
`turn`, `rotatezoom`, `rotatevanish`, `rotateswap` and `ripple`
(`reference/cpp/plugins/wave.cpp`, `mosaic.cpp`, `turn.cpp`,
`rotatetrans.cpp`, `ripple.cpp`), alongside the engine's built-in `crossfade`,
`universal` and `scroll`. The port dispatches the same set, plus the aliases
the reference's lookup accepts and the `mask` form of `universal`:

| name | reference source | behaviour |
|---|---|---|
| `crossfade`, `fade`, `""` | engine built-in | the default cross-fade; the empty name is the KAG default method, and the reference's provider lookup misses the empty key and falls back to `crossfade` (`reference/cpp/core/visual/TransIntf.cpp:331-341`) |
| `scroll`, `slide`, `push` | engine built-in | a directional scroll |
| `universal` | engine built-in | the universal rule-image transition |
| `mask` | — | the reference's `universal` with a hard threshold |
| `wave` | `wave.cpp` | the wave transition |
| `mosaic` | `mosaic.cpp` | the mosaic transition |
| `turn` | `turn.cpp` | the turn transition |
| `rotatezoom` | `rotatetrans.cpp` | rotate + zoom |
| `rotatevanish` | `rotatetrans.cpp` | rotate + vanish |
| `rotateswap` | `rotatetrans.cpp` | rotate + swap |
| `ripple` | `ripple.cpp` | the ripple transition |

Every option is read through the shared option helpers and validated inside
the engine, so an out-of-range value is a typed error rather than a silent
clamp. The defaults are the reference's (`wave.cpp:327-331`,
`mosaic.cpp:436-445`, `turn.cpp:555-563`,
`rotatetrans.cpp:183-193,284-291,453-465`, `ripple.cpp:1574-1627`).

## State and lifetime

No state: the module registers nothing. The transition state lives in the
engine's `Trans` value, which the caller owns.

## Errors

None from this module. A transition name the dispatcher does not know, or an
option outside its range, is reported by the transition dispatcher as a typed
error.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/transitions.rs` | each transition provider's output and its option handling |
| `crates/compat/tests/layer.rs` | `Layer.beginTransition` dispatch |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("extrans.dll")` resolves to the built-in table |

## Known gaps

* The reference registers its providers into a **global** registry that a
  script could, in principle, extend. The port's dispatcher is a fixed
  match, so a script cannot add a transition provider of its own. No
  reference plugin exposes that registry to TJS, so nothing script-visible
  changes.

# Native surface parity

> **Generated** by `krkr-gate native-parity` on 2026-10-25 — do not edit by hand.

Machine-diff of the reference C++ native class surface (`reference/cpp/core/**/*.cpp`) against the native classes the Rust engine registers. A *missing* member exists in the reference but is not registered by Rust; an *extra* member is registered by Rust but has no reference counterpart (engine-internal helpers such as `id` and `nativeId` are expected here).

## Usage

```sh
uv run --project scripts krkr-gate native-parity                 # check + regenerate this report
uv run --project scripts krkr-gate native-parity --no-doc        # check only
uv run --project scripts krkr-gate native-parity --update-allow  # accept current gaps after review
uv run --project scripts krkr-gate native-parity --update-props-allow  # accept accessor mismatches
uv run --project scripts krkr-gate native-parity --self-test     # extractor fixture self-test
```

The checker exits non-zero when a missing member is **not** listed in `scripts/allowlists/native_parity_allow.txt` (a regression). When an allowlisted member is implemented it prints a stale-entry warning so the allowlist can shrink. Coverage can therefore only move in one direction.

## Coverage summary

| metric | value |
| --- | ---: |
| tracked classes | 25 |
| reference members | 660 |
| matched | 660 |
| **missing** | **0** |
| extra (Rust only) | 5 |
| coverage | **100.0%** |

## Reference registration mechanisms

How each reference member becomes script-visible. `native-macro` is `TJS_BEGIN_NATIVE_*_DECL`, `constructor` the class-name entry, and `propset` a class-object member installed after the class is built with `PropSet(TJS_MEMBERENSURE, ...)`. A member reached by more than one mechanism is counted once, under the strongest.

| mechanism | members |
| --- | ---: |
| `native-macro` | 756 |
| `constructor` | 33 |
| `propset` | 4 |

## Class-level members installed by `PropSet`

Members and class objects attached to an already-built class object. `kind` is `unknown` when `PropSet` does not reveal method versus property.

| class | member | kind | static | mechanism |
| --- | --- | --- | --- | --- |
| `WaveSoundBuffer` | `PhaseVocoder` | unknown | true | `propset` |
| `Window` | `BasicDrawDevice` | unknown | true | `propset` |
| `Window` | `PassThroughDrawDevice` | unknown | true | `propset` |
| `Window` | `menu` | unknown | false | `propset` |

## Registration sites with a runtime-built name

_None._

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Layer` | 149 | 151 | 149 | 0 | 2 | 100.0% |
| `Bitmap` | 19 | 19 | 19 | 0 | 0 | 100.0% |
| `Window` | 91 | 91 | 91 | 0 | 0 | 100.0% |
| `Font` | 21 | 21 | 21 | 0 | 0 | 100.0% |
| `Timer` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `System` | 49 | 49 | 49 | 0 | 0 | 100.0% |
| `Debug` | 12 | 12 | 12 | 0 | 0 | 100.0% |
| `Plugins` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `VideoOverlay` | 69 | 69 | 69 | 0 | 0 | 100.0% |
| `AsyncTrigger` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `MenuItem` | 21 | 21 | 21 | 0 | 0 | 100.0% |
| `Storages` | 13 | 13 | 13 | 0 | 0 | 100.0% |
| `Scripts` | 10 | 10 | 10 | 0 | 0 | 100.0% |
| `KAGParser` | 24 | 26 | 24 | 0 | 2 | 100.0% |
| `WaveSoundBuffer` | 33 | 33 | 33 | 0 | 0 | 100.0% |
| `Pad` | 24 | 24 | 24 | 0 | 0 | 100.0% |
| `Rect` | 19 | 19 | 19 | 0 | 0 | 100.0% |
| `Clipboard` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `ImageFunction` | 12 | 12 | 12 | 0 | 0 | 100.0% |
| `BitmapLayerTreeOwner` | 36 | 36 | 36 | 0 | 0 | 100.0% |
| `CDDASoundBuffer` | 14 | 14 | 14 | 0 | 0 | 100.0% |
| `MIDISoundBuffer` | 15 | 15 | 15 | 0 | 0 | 100.0% |
| `BasicDrawDevice` | 4 | 4 | 4 | 0 | 0 | 100.0% |
| `PassThroughDrawDevice` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `PhaseVocoder` | 5 | 6 | 5 | 0 | 1 | 100.0% |

## Missing members by class

_None for tracked classes._
## Extra members by class

Rust registrations with no reference counterpart; usually intentional engine internals.

### `Layer` — 2 extra

`getTextHeight`, `getTextWidth`

### `KAGParser` — 2 extra

`multiLineTagEnabled`, `paramMacros`

### `PhaseVocoder` — 1 extra

`__TvpPhaseVocoder`

## Reference classes not registered in Rust

| class | reference members |
| --- | ---: |
| `WaveFlags` | 3 |

## Rust-only registered classes

These builders have no native C++ counterpart in `reference/cpp/core` (TJS-level or plugin classes).

| class | Rust members | source |
| --- | ---: | --- |
| `SoundBuffer` | 0 | `crates/compat/src/natives/sound.rs#SoundBuffer` |
| `SoundChannel` | 0 | `crates/compat/src/natives/sound.rs#SoundChannel` |

## Extraction notes and remaining blind spots

- Reference members come from `TJS_BEGIN_NATIVE_(STATIC_)METHOD/PROP_DECL` blocks, `TJS_END_NATIVE_CONSTRUCTOR_DECL`, and class-object members installed with `PropSet`/`PropSetByNum` on a variable bound by `TVPCreateNativeClass_*`/`registerObject`.
- The class-object (`TJS_STATICMEMBER`) kind comes from the `TJS_END_NATIVE_STATIC_*` closing macro or a trailing `/* static */` comment; a `PropSet` kind comes from the `TJS_STATICMEMBER` flag in the mode argument.
- The Rust side is read from the builders in `RUST_CLASS_MAP`, plus `attach_class(ctx, "Target", "Member")` and a class member assigned inside an `exec_script` string, so a class attached after construction is not a false missing.
- A `PropSet` name that is not a string literal is not counted (see the runtime-built-name section): a name built at runtime cannot be recovered from source, and a coverage figure that silently dropped it would read as evidence it does not exist.
- A `PropSet` on an *instance* object (`this`, a dictionary, a method parameter) is not a class member and is deliberately not counted; only class-object receivers are members of the class surface.
- `global->PropSet(...)` registers a global (a class object or a free function such as `getDirList`), not a class member; the plugin reader covers that surface, this report does not.
- A member installed by a plugin at load time rather than in the core is outside this report; `krkr-gate plugin-parity` covers the plugin surface.

## Property getter/setter parity

A reference property declared with `TJS_BEGIN_NATIVE_PROP_DECL` must expose the same accessors in Rust: a reference-readable property needs a Rust `get`, and a reference-writable one a Rust `set`. The reverse (a Rust accessor the reference denies) is listed as *extra* and fails identically. `scripts/allowlists/native_parity_props_allow.txt` records accepted exceptions.

- missing accessors: 0
- extra accessors: 0

_None._

## Static/instance kind parity

A reference member that lives on the class object (`TJS_STATICMEMBER`) must be registered by Rust on the class object, and an instance member on the instance; the reverse dispatches the wrong native callback shape (the `Window.mainWindow` crash). `scripts/allowlists/native_parity_static_allow.txt` records accepted exceptions.

- mismatches: 0

_None._

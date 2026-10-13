# Plugin surface parity

> **Generated** by `scripts/plugin_parity.py` on 2026-10-13 — do not edit by hand.

Machine-diff of the reference KiriKiri plugin surface (`reference/cpp/plugins/**`) against the plugin classes the Rust engine registers (`crates/compat/src/natives/plugins/**`). A *missing* member exists in the reference but is not registered by Rust. An *extra* is a genuine plugin-surface addition: a member registered by a plugin file that the reference class does not have. Core natives (`natives/*.rs`) and members inherited from a shared registration table are counted separately.

## Usage

```sh
python3 scripts/plugin_parity.py                 # check + regenerate this report
python3 scripts/plugin_parity.py --no-doc        # check only
python3 scripts/plugin_parity.py --update-allow  # accept current gaps after review
python3 scripts/plugin_parity.py --self-test     # extractor fixture self-test
```

The checker exits non-zero when a missing member is **not** listed in `scripts/plugin_parity_allow.txt` (a regression). When an allowlisted member is implemented it prints a stale-entry warning so the allowlist can shrink. Coverage can therefore only move in one direction.

## Coverage summary

| metric | value |
| --- | ---: |
| tracked classes | 56 |
| reference members | 974 |
| matched | 974 |
| **missing** | **0** |
| extra (genuine plugin additions) | 0 |
| core members (not plugin extras) | 336 |
| coverage | **100.0%** |

## Members installed by `PropSet`

Class and global members the reference installs after construction with ``PropSet(TJS_MEMBERENSURE, ...)``: a namespace attaching a class (``Motion.Player``), the global object registering a free function (``getDirList``), or a class-object member. ``kind`` is unknown because ``PropSet`` does not reveal method versus property.

| class | member | kind | static | mechanism |
| --- | --- | --- | --- | --- |
| `Motion` | `Player` | unknown | true | `propset` |
| `__global__` | `getDirList` | unknown | true | `propset` |

## Registration sites with a runtime-built name

2 site(s) install a member whose name is not a string literal, or whose receiver is a helper parameter, so the source reader cannot recover it. These are not counted as members: the coverage figures exclude them, which is a remaining blind spot, not a claim they do not exist.

- `reference/cpp/plugins/layerExPerspective.cpp: Layer.dispatch.PropSet: methodName`
- `reference/cpp/plugins/motionplayer/main.cpp: unresolved static member playerClass.PropSet: useD3D`

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Array` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `AsyncTrigger` | 0 | 5 | 0 | 0 | 0 | n/a |
| `BitmapLayerTreeOwner` | 0 | 36 | 0 | 0 | 0 | n/a |
| `CSVParser` | 6 | 6 | 6 | 0 | 0 | 100.0% |
| `Clipboard` | 0 | 2 | 0 | 0 | 0 | n/a |
| `Console` | 7 | 8 | 7 | 0 | 0 | 100.0% |
| `Controller` | 0 | 1 | 0 | 0 | 0 | n/a |
| `D3DEmoteModule` | 9 | 9 | 9 | 0 | 0 | 100.0% |
| `D3DEmotePlayer` | 87 | 181 | 87 | 0 | 0 | 100.0% |
| `Debug` | 0 | 12 | 0 | 0 | 0 | n/a |
| `Dictionary` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `DrawDeviceForSteam` | 1 | 1 | 1 | 0 | 0 | 100.0% |
| `GdiPlus` | 157 | 157 | 157 | 0 | 0 | 100.0% |
| `GdiPlus.Appearance` | 3 | 4 | 3 | 0 | 0 | 100.0% |
| `GdiPlus.Font` | 9 | 10 | 9 | 0 | 0 | 100.0% |
| `GdiPlus.Image` | 0 | 34 | 0 | 0 | 0 | n/a |
| `GdiPlus.Matrix` | 0 | 16 | 0 | 0 | 0 | n/a |
| `GdiPlus.Path` | 17 | 18 | 17 | 0 | 0 | 100.0% |
| `GdiPlus.PointF` | 3 | 4 | 3 | 0 | 0 | 100.0% |
| `GdiPlus.RectF` | 18 | 19 | 18 | 0 | 0 | 100.0% |
| `ImageFunction` | 0 | 12 | 0 | 0 | 0 | n/a |
| `Key` | 0 | 4 | 0 | 0 | 0 | n/a |
| `Layer` | 48 | 48 | 48 | 0 | 0 | 100.0% |
| `MIDISoundBuffer` | 0 | 1 | 0 | 0 | 0 | n/a |
| `MenuItem` | 15 | 36 | 15 | 0 | 0 | 100.0% |
| `Motion` | 23 | 23 | 23 | 0 | 0 | 100.0% |
| `Motion.Circle` | 5 | 6 | 5 | 0 | 0 | 100.0% |
| `Motion.D3DAdaptor` | 15 | 16 | 15 | 0 | 0 | 100.0% |
| `Motion.EmotePlayer` | 88 | 181 | 88 | 0 | 0 | 100.0% |
| `Motion.LayerGetter` | 27 | 28 | 27 | 0 | 0 | 100.0% |
| `Motion.ObjSource` | 0 | 1 | 0 | 0 | 0 | n/a |
| `Motion.Point` | 4 | 5 | 4 | 0 | 0 | 100.0% |
| `Motion.Quad` | 3 | 4 | 3 | 0 | 0 | 100.0% |
| `Motion.Rect` | 6 | 6 | 6 | 0 | 0 | 100.0% |
| `Motion.ResourceManager` | 9 | 10 | 9 | 0 | 0 | 100.0% |
| `Motion.SeparateLayerAdaptor` | 5 | 6 | 5 | 0 | 0 | 100.0% |
| `Motion.SourceCache` | 3 | 4 | 3 | 0 | 0 | 100.0% |
| `Mouse` | 0 | 14 | 0 | 0 | 0 | n/a |
| `PSBFile` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `PSD` | 59 | 59 | 59 | 0 | 0 | 100.0% |
| `Pad` | 1 | 25 | 1 | 0 | 0 | 100.0% |
| `Player` | 141 | 141 | 141 | 0 | 0 | 100.0% |
| `Rect` | 0 | 25 | 0 | 0 | 0 | n/a |
| `Scripts` | 24 | 33 | 24 | 0 | 0 | 100.0% |
| `Steam` | 23 | 23 | 23 | 0 | 0 | 100.0% |
| `Storages` | 32 | 45 | 32 | 0 | 0 | 100.0% |
| `System` | 16 | 64 | 16 | 0 | 0 | 100.0% |
| `TemporaryFiles` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `TextRenderBase` | 48 | 48 | 48 | 0 | 0 | 100.0% |
| `VideoOverlay` | 0 | 69 | 0 | 0 | 0 | n/a |
| `WIN32Dialog` | 1 | 1 | 1 | 0 | 0 | 100.0% |
| `WaveSoundBuffer` | 6 | 38 | 6 | 0 | 0 | 100.0% |
| `Window` | 44 | 44 | 44 | 0 | 0 | 100.0% |
| `__global__` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `open` | 0 | 13 | 0 | 0 | 0 | n/a |
| `steam_api` | 0 | 0 | 0 | 0 | 0 | n/a |

## Namespace structure

The reference publishes a class object with ``NCB_REGISTER_CLASS`` and attaches classes inside it with ``NCB_SUBCLASS`` (script name ``Namespace.name``); ``NCB_REGISTER_SUBCLASS`` alone never publishes a global. A missing namespace object or nested class is an allowlistable ratchet gap (like a missing member). A nested class exposed as a bare global, or a top-level class the reference never publishes, is a hard failure: that is the registration bug class.

### Missing namespace / nested classes (allowlistable)

_None._

### Wrong-level classes (hard failures)

_None._

## Missing members by class

_None for tracked classes._
## Extra members by class

Genuine plugin-surface additions: members registered in a plugin file that the reference class does not have. Core natives and members inherited from a shared registration table (``EmotePlayer`` gets ``Player``'s table) are excluded.

_None for tracked classes._
## Core members by class (excluded from extras)

Members registered by a core file (``crates/compat/src/natives/*.rs``) with no reference-plugin counterpart. These are not plugin-surface additions.

| class | core members |
| --- | ---: |
| `AsyncTrigger` | 5 |
| `BitmapLayerTreeOwner` | 36 |
| `Clipboard` | 2 |
| `Console` | 1 |
| `Controller` | 1 |
| `Debug` | 12 |
| `ImageFunction` | 12 |
| `Key` | 4 |
| `MIDISoundBuffer` | 1 |
| `MenuItem` | 21 |
| `Mouse` | 14 |
| `Pad` | 24 |
| `Rect` | 19 |
| `Scripts` | 9 |
| `Storages` | 13 |
| `System` | 48 |
| `VideoOverlay` | 69 |
| `WaveSoundBuffer` | 32 |
| `open` | 13 |

## Rust-only registered classes

Plugin classes Rust registers that have no reference counterpart reachable through the ncbind surface (TJS aliases, GDI+ internals, or plugins whose reference file uses manual TJS registration).

| class | Rust members | source |
| --- | ---: | --- |
| `AsyncTrigger` | 5 | `crates/compat/src/natives/async_trigger.rs` |
| `BitmapLayerTreeOwner` | 36 | `crates/compat/src/natives/tree_owner.rs` |
| `Clipboard` | 2 | `crates/compat/src/natives/clipboard.rs` |
| `Controller` | 1 | `crates/compat/src/natives/debug.rs` |
| `Debug` | 12 | `crates/compat/src/natives/debug.rs` |
| `GdiPlus.Image` | 34 |  |
| `GdiPlus.Matrix` | 16 |  |
| `ImageFunction` | 12 | `crates/compat/src/natives/image_function.rs` |
| `Key` | 4 | `crates/compat/src/natives/input.rs` |
| `MIDISoundBuffer` | 1 | `crates/compat/src/natives/cdda_midi.rs` |
| `Mouse` | 14 | `crates/compat/src/natives/input.rs` |
| `Rect` | 25 | `crates/compat/src/natives/plugins/motion_player/adaptors.rs`, `crates/compat/src/natives/rect.rs` |
| `VideoOverlay` | 69 | `crates/compat/src/natives/video.rs` |
| `open` | 13 | `crates/compat/src/natives/cdda_midi.rs` |

## Extraction notes

- Core native classes (`Storages`, `System`, `Scripts`, ...) are read from `crates/compat/src/natives/*.rs` as well as the plugin tree, so plugin registrations that attach to a core class are matched.
- `NCB_ATTACH_CLASS*` members are also recorded under the C++ implementation class and matched to a Rust class with the canonicalised name (`layerExMovie` -> `LayerExMovie`). Manual `addMethod(dispatch, TJS_W("name"), ...)` registrations are read the same way.
- `MotionRect` is compared against the reference `Rect` (the Rust port renames it to avoid colliding with the core `Rect` class).
- `layerExBase` is not modelled as a class: it is an internal C++ base whose names (`imageWidth`, `clipLeft`, `update`, ...) are read from the `Layer` object (`reference/cpp/plugins/layerExBase.hpp:84-91`) and are part of the core `Layer` surface, which `native_parity.py` covers. `LayerExMovie`/`LayerExPerspective` are `NCB_ATTACH_CLASS` implementations, not published classes.
- `NCB_SUBCLASS`/`NCB_SUBCLASS_NAME`/`NCB_GDIP_SUBCLASS`/`SubClass` inside a `NCB_REGISTER_CLASS` block are modelled as `Namespace.name` classes. A Rust port that registers the class globally and then `set_member`s it into the namespace and `delete_member`s the global (`motion_player`) is recognised as the nested class.
- A plugin file that registers a class the reference never publishes at all (`LayerExMovie`, `LayerExPerspective`, a bare `LayerExBase`) is a hard failure, as is a nested class leaking to the top level. A missing namespace object or nested class is an allowlistable ratchet gap.
- Class and global members installed after construction with `PropSet(TJS_MEMBERENSURE, ...)` are read too: a namespace attaching a class (`motion->PropSet("Player")` -> `Motion.Player`), the global object registering a free function (`getDirList`), and a TJS global function published from a `TVPExecuteScript` string (`drawFFTGraph`).
- Remaining blind spots: a `PropSet` name built at runtime (`idstr.c_str()`) or a static member on an unresolved helper parameter (`playerClass->PropSet(...)`) is recorded as unrecoverable, not counted; the coverage figures therefore exclude it rather than claiming it absent.
- A `PropSet` on an instance object (a window, a dictionary, a method parameter) is not a class member and is not counted. A member installed by a plugin DLL that has no `reference/cpp` source is outside this report until its surface is extracted (D38).

## Documented exclusions

The following macros are not script surface and are intentionally not parsed as members:

- `NCB_MODULE_NAME`
- `NCB_PRE_REGIST_CALLBACK`
- `NCB_POST_REGIST_CALLBACK`
- `NCB_PRE_UNREGIST_CALLBACK`
- `NCB_POST_UNREGIST_CALLBACK`
- `NCB_SET_CONVERTOR / _SRC / _DST / _BOTH`
- `NCB_TYPECONV_* (boxing/convertor wiring)`
- `NCB_GDIP_* (GDI+ rendering internals)`

No reference classes are excluded: `GdiPlus` and its ``ENUM`` constants are part of the reference surface and are checked like any other namespace. `Debug.console` targets are reported as the Rust class `Console`. The `finalize` lifecycle hook is ignored on both sides.

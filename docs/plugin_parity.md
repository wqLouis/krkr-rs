# Plugin surface parity

> **Generated** by `scripts/plugin_parity.py` on 2026-10-09 — do not edit by hand.

Machine-diff of the reference KiriKiri plugin surface (`reference/cpp/plugins/**`) against the plugin classes the Rust engine registers (`crates/compat/src/natives/plugins/**`). A *missing* member exists in the reference but is not registered by Rust. An *extra* is a genuine plugin-surface addition: a member registered by a plugin file that the reference class does not have. Core natives (`natives/*.rs`) and members inherited from a shared registration table are counted separately.

## Usage

```sh
python3 scripts/plugin_parity.py                 # check + regenerate this report
python3 scripts/plugin_parity.py --no-doc        # check only
python3 scripts/plugin_parity.py --update-allow  # accept current gaps after review
```

The checker exits non-zero when a missing member is **not** listed in `scripts/plugin_parity_allow.txt` (a regression). When an allowlisted member is implemented it prints a stale-entry warning so the allowlist can shrink. Coverage can therefore only move in one direction.

## Coverage summary

| metric | value |
| --- | ---: |
| tracked classes | 54 |
| reference members | 828 |
| matched | 828 |
| **missing** | **0** |
| extra (genuine plugin additions) | 68 |
| core members (not plugin extras) | 336 |
| coverage | **100.0%** |

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Appearance` | 3 | 4 | 3 | 0 | 1 | 100.0% |
| `Array` | 3 | 4 | 3 | 0 | 1 | 100.0% |
| `AsyncTrigger` | 0 | 5 | 0 | 0 | 0 | n/a |
| `BitmapLayerTreeOwner` | 0 | 36 | 0 | 0 | 0 | n/a |
| `CSVParser` | 6 | 6 | 6 | 0 | 0 | 100.0% |
| `Circle` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `Clipboard` | 0 | 2 | 0 | 0 | 0 | n/a |
| `Console` | 7 | 8 | 7 | 0 | 0 | 100.0% |
| `Controller` | 0 | 1 | 0 | 0 | 0 | n/a |
| `D3DAdaptor` | 15 | 15 | 15 | 0 | 0 | 100.0% |
| `D3DEmoteModule` | 9 | 10 | 9 | 0 | 1 | 100.0% |
| `D3DEmotePlayer` | 87 | 181 | 87 | 0 | 0 | 100.0% |
| `Debug` | 0 | 12 | 0 | 0 | 0 | n/a |
| `Dictionary` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `DrawDeviceForSteam` | 1 | 1 | 1 | 0 | 0 | 100.0% |
| `EmotePlayer` | 88 | 181 | 88 | 0 | 0 | 100.0% |
| `FontInfo` | 9 | 9 | 9 | 0 | 0 | 100.0% |
| `ImageFunction` | 0 | 12 | 0 | 0 | 0 | n/a |
| `Key` | 0 | 4 | 0 | 0 | 0 | n/a |
| `Layer` | 48 | 51 | 48 | 0 | 3 | 100.0% |
| `LayerExBase` | 11 | 13 | 11 | 0 | 2 | 100.0% |
| `LayerExMovie` | 4 | 20 | 4 | 0 | 16 | 100.0% |
| `LayerExPerspective` | 1 | 14 | 1 | 0 | 13 | 100.0% |
| `LayerGetter` | 27 | 27 | 27 | 0 | 0 | 100.0% |
| `MIDISoundBuffer` | 0 | 1 | 0 | 0 | 0 | n/a |
| `MenuItem` | 15 | 36 | 15 | 0 | 0 | 100.0% |
| `Motion` | 23 | 23 | 23 | 0 | 0 | 100.0% |
| `Mouse` | 0 | 14 | 0 | 0 | 0 | n/a |
| `ObjSource` | 0 | 0 | 0 | 0 | 0 | n/a |
| `PSBFile` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `PSD` | 59 | 59 | 59 | 0 | 0 | 100.0% |
| `Pad` | 1 | 25 | 1 | 0 | 0 | 100.0% |
| `Path` | 17 | 18 | 17 | 0 | 1 | 100.0% |
| `Player` | 141 | 141 | 141 | 0 | 0 | 100.0% |
| `Point` | 4 | 4 | 4 | 0 | 0 | 100.0% |
| `PointF` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `Quad` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `Rect` | 6 | 25 | 6 | 0 | 0 | 100.0% |
| `RectF` | 18 | 18 | 18 | 0 | 0 | 100.0% |
| `ResourceManager` | 9 | 9 | 9 | 0 | 0 | 100.0% |
| `Scripts` | 24 | 33 | 24 | 0 | 0 | 100.0% |
| `SeparateLayerAdaptor` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `SourceCache` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `Steam` | 23 | 23 | 23 | 0 | 0 | 100.0% |
| `Storages` | 30 | 45 | 30 | 0 | 2 | 100.0% |
| `System` | 16 | 64 | 16 | 0 | 0 | 100.0% |
| `TemporaryFiles` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `TextRenderBase` | 48 | 48 | 48 | 0 | 0 | 100.0% |
| `VideoOverlay` | 0 | 69 | 0 | 0 | 0 | n/a |
| `WIN32Dialog` | 0 | 1 | 0 | 0 | 1 | n/a |
| `WaveSoundBuffer` | 6 | 38 | 6 | 0 | 0 | 100.0% |
| `Window` | 44 | 71 | 44 | 0 | 27 | 100.0% |
| `open` | 0 | 13 | 0 | 0 | 0 | n/a |
| `steam_api` | 0 | 0 | 0 | 0 | 0 | n/a |

## Missing members by class

_None for tracked classes._
## Extra members by class

Genuine plugin-surface additions: members registered in a plugin file that the reference class does not have. Core natives and members inherited from a shared registration table (``EmotePlayer`` gets ``Player``'s table) are excluded.

### `Appearance` — 1 extra

`__handle`

### `Array` — 1 extra

`xor`

### `D3DEmoteModule` — 1 extra

`maxTextureSize`

### `Layer` — 3 extra

`onStartMovie`, `onStopMovie`, `onUpdateMovie`

### `LayerExBase` — 2 extra

`imageLeft`, `imageTop`

### `LayerExMovie` — 16 extra

`clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `imageHeight`, `imageLeft`, `imageTop`, `imageWidth`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `onStartMovie`, `onStopMovie`, `onUpdateMovie`, `redraw`, `reset`, `update`

### `LayerExPerspective` — 13 extra

`clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `imageHeight`, `imageLeft`, `imageTop`, `imageWidth`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `redraw`, `reset`, `update`

### `Path` — 1 extra

`__handle`

### `Storages` — 2 extra

`setXP3ArchiveContentFilter`, `setXP3ArchiveExtractionFilter`

### `WIN32Dialog` — 1 extra

`messageBox`

### `Window` — 27 extra

`disableMove`, `onAccelKeyMenu`, `onActivateChanged`, `onDisplayChanged`, `onEnterMenuLoop`, `onExSystemMenuSelected`, `onExitMenuLoop`, `onHide`, `onMaximize`, `onMaximizeQuery`, `onMinimize`, `onMonitorPower`, `onMove`, `onMoveSizeBegin`, `onMoveSizeEnd`, `onMoving`, `onNcMouseDown`, `onNcMouseLeave`, `onNcMouseMove`, `onNcMouseUp`, `onNonCapMouseEvent`, `onResizing`, `onScreenSave`, `onShow`, `onStartKeyMenu`, `onWindowsMessageHook`, `resetExSystemMenu`

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
| `ImageFunction` | 12 | `crates/compat/src/natives/image_function.rs` |
| `Key` | 4 | `crates/compat/src/natives/input.rs` |
| `MIDISoundBuffer` | 1 | `crates/compat/src/natives/cdda_midi.rs` |
| `Mouse` | 14 | `crates/compat/src/natives/input.rs` |
| `VideoOverlay` | 69 | `crates/compat/src/natives/video.rs` |
| `WIN32Dialog` | 1 | `crates/compat/src/natives/plugins/win32_dialog/mod.rs` |
| `open` | 13 | `crates/compat/src/natives/cdda_midi.rs` |

## Extraction notes

- Core native classes (`Storages`, `System`, `Scripts`, ...) are read from `crates/compat/src/natives/*.rs` as well as the plugin tree, so plugin registrations that attach to a core class are matched.
- `NCB_ATTACH_CLASS*` members are also recorded under the C++ implementation class and matched to a Rust class with the canonicalised name (`layerExMovie` -> `LayerExMovie`). Manual `addMethod(dispatch, TJS_W("name"), ...)` registrations are read the same way.
- `MotionRect` is compared against the reference `Rect` (the Rust port renames it to avoid colliding with the core `Rect` class).
- `LayerExBase` has no registered TJS class in the reference; its surface comes from the public methods and `TJS_W("...")` property names of the `layerExBase` helper struct.

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

Reference classes in `GdiPlus` are skipped entirely (GDI+ rendering internals). `Debug.console` targets are reported as the Rust class `Console`. The `finalize` lifecycle hook is ignored on both sides.

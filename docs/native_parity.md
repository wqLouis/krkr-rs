# Native surface parity

> **Generated** by `scripts/native_parity.py` on 2026-09-14 — do not edit by hand.

Machine-diff of the reference C++ native class surface (`reference/cpp/core/**/*.cpp`) against the native classes the Rust engine registers. A *missing* member exists in the reference but is not registered by Rust; an *extra* member is registered by Rust but has no reference counterpart (engine-internal helpers such as `id` and `nativeId` are expected here).

## Usage

```sh
python3 scripts/native_parity.py                 # check + regenerate this report
python3 scripts/native_parity.py --no-doc        # check only
python3 scripts/native_parity.py --update-allow  # accept current gaps after review
```

The checker exits non-zero when a missing member is **not** listed in `scripts/native_parity_allow.txt` (a regression). When an allowlisted member is implemented it prints a stale-entry warning so the allowlist can shrink. Coverage can therefore only move in one direction.

## Coverage summary

| metric | value |
| --- | ---: |
| tracked classes | 15 |
| reference members | 527 |
| matched | 474 |
| **missing** | **53** |
| extra (Rust only) | 105 |
| coverage | **89.9%** |

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Layer` | 150 | 169 | 124 | 26 | 45 | 82.7% |
| `Bitmap` | 20 | 22 | 20 | 0 | 2 | 100.0% |
| `Window` | 83 | 91 | 83 | 0 | 8 | 100.0% |
| `Font` | 22 | 14 | 11 | 11 | 3 | 50.0% |
| `Timer` | 6 | 7 | 5 | 1 | 2 | 83.3% |
| `System` | 50 | 52 | 50 | 0 | 2 | 100.0% |
| `Debug` | 13 | 11 | 11 | 2 | 0 | 84.6% |
| `Plugins` | 3 | 4 | 3 | 0 | 1 | 100.0% |
| `VideoOverlay` | 70 | 75 | 66 | 4 | 9 | 94.3% |
| `AsyncTrigger` | 6 | 6 | 6 | 0 | 0 | 100.0% |
| `MenuItem` | 22 | 19 | 19 | 3 | 0 | 86.4% |
| `Storages` | 13 | 20 | 13 | 0 | 7 | 100.0% |
| `Scripts` | 11 | 7 | 7 | 4 | 0 | 63.6% |
| `KAGParser` | 25 | 43 | 23 | 2 | 20 | 92.0% |
| `WaveSoundBuffer` | 33 | 39 | 33 | 0 | 6 | 100.0% |

## Missing members by class

### `Layer` — 26 missing

`children`, `getLayerAt`, `joinFocusChain`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `nextFocusable`, `onBeforeFocus`, `onBlur`, `onFocus`, `onKeyPress`, `onMultiTouch`, `onNodeDisabled`, `onNodeEnabled`, `onSearchNextFocusable`, `onSearchPrevFocusable`, `onTouchDown`, `onTouchMove`, `onTouchRotate`, `onTouchScaling`, `onTouchUp`, `onTransitionCompleted`, `prevFocusable`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`

### `Font` — 11 missing

`defaultFaceName`, `doUserSelect`, `faceIsFileName`, `getEscHeightX`, `getEscHeightY`, `getEscWidthX`, `getEscWidthY`, `getGlyphDrawRect`, `getList`, `rasterizer`, `unmapPrerenderedFont`

### `Timer` — 1 missing

`mode`

### `Debug` — 2 missing

`console`, `controller`

### `VideoOverlay` — 4 missing

`onCallbackCommand`, `onFrameUpdate`, `onPeriod`, `onStatusChanged`

### `MenuItem` — 3 missing

`HMENU`, `keycodeToText`, `textToKeycode`

### `Scripts` — 4 missing

`compileStorage`, `dump`, `getClassNames`, `setCallMissing`

### `KAGParser` — 2 missing

`macroParams`, `mp`

## Extra members by class

Rust registrations with no reference counterpart; usually intentional engine internals.

### `Layer` — 45 extra

`captureMouse`, `captureTouch`, `clear`, `colorize`, `doBlurLight`, `doDropShadow`, `drawArc`, `drawBezier`, `drawBeziers`, `drawClosedCurve`, `drawClosedCurve2`, `drawCurve`, `drawCurve2`, `drawCurve3`, `drawEllipse`, `drawImage`, `drawImageAffine`, `drawImageRect`, `drawImageStretch`, `drawLine`, `drawLines`, `drawPath`, `drawPie`, `drawPolygon`, `drawRectangle`, `drawRectangles`, `drawString`, `fillOperateRect`, `getDrawWidth`, `getList`, `getTextHeight`, `getTextWidth`, `id`, `moveToFront`, `nativeId`, `noise`, `resetDrawTextParam`, `setAffineOffset`, `setBitmap`, `setCenter`, `setDefaultDrawTextParam`, `setFontStyle`, `setImage`, `setParentId`, `tileRect`

### `Bitmap` — 2 extra

`id`, `nativeId`

### `Window` — 8 extra

`addInputNotify`, `changeScreenMode`, `id`, `innerSunken`, `menu`, `registerExEvent`, `showScrollBars`, `zoom`

### `Font` — 3 extra

`__bind`, `color`, `id`

### `Timer` — 2 extra

`count`, `id`

### `System` — 2 extra

`getDisplayMonitors`, `getMonitorInfo`

### `Plugins` — 1 extra

`Plugins`

### `VideoOverlay` — 9 extra

`audioChannels`, `audioSampleCount`, `audioSampleRate`, `frameBytes`, `frameChecksum`, `frameHeight`, `frameWidth`, `setTransitionCompleteCall`, `totalFrame`

### `Storages` — 7 extra

`Storages`, `copyFile`, `deleteFile`, `fstat`, `getFileList`, `open`, `stat`

### `KAGParser` — 20 extra

`getCallStackDepth`, `getCurLabel`, `getCurLine`, `getCurLineStr`, `getCurPos`, `getCurStorage`, `getDebugLevel`, `getIgnoreCR`, `getMP`, `getMacroParams`, `getMacros`, `getMultiLineTagEnabled`, `getProcessSpecialTags`, `multiLineTagEnabled`, `setCurStorage`, `setDebugLevel`, `setIgnoreCR`, `setMacros`, `setMultiLineTagEnabled`, `setProcessSpecialTags`

### `WaveSoundBuffer` — 6 extra

`fadeIn`, `fadeOut`, `getStatus`, `pause`, `resume`, `speed`

## Reference classes not registered in Rust

| class | reference members |
| --- | ---: |
| `BasicDrawDevice` | 6 |
| `BitmapLayerTreeOwner` | 37 |
| `CDDASoundBuffer` | 15 |
| `Clipboard` | 3 |
| `ImageFunction` | 13 |
| `MIDISoundBuffer` | 16 |
| `Pad` | 25 |
| `PassThroughDrawDevice` | 7 |
| `PhaseVocoder` | 6 |
| `Rect` | 20 |
| `WaveFlags` | 4 |

## Rust-only registered classes

These builders have no native C++ counterpart in `reference/cpp/core` (TJS-level or plugin classes).

| class | Rust members | source |
| --- | ---: | --- |
| `SoundBuffer` | 5 | `crates/tvp-sound/src/natives.rs#SoundBuffer` |
| `SoundChannel` | 19 | `crates/tvp-sound/src/natives.rs#SoundChannel` |
| `GdiPlusAppearance` | 4 | `crates/tvp-visual/src/natives/gdiplus.rs#GdiPlusAppearance` |
| `ChainItemBase` | 1 | `crates/tvp-natives/src/chain_item_base.rs` |
| `Trans` | 2 | `crates/tvp-natives/src/extrans.rs` |

# Native surface parity

> **Generated** by `scripts/native_parity.py` on 2026-10-07 — do not edit by hand.

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
| matched | 249 |
| **missing** | **278** |
| extra (Rust only) | 575 |
| coverage | **47.2%** |

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Layer` | 150 | 139 | 75 | 75 | 64 | 50.0% |
| `Bitmap` | 20 | 139 | 15 | 5 | 124 | 75.0% |
| `Window` | 83 | 139 | 43 | 40 | 96 | 51.8% |
| `Font` | 22 | 139 | 9 | 13 | 130 | 40.9% |
| `Timer` | 6 | 139 | 6 | 0 | 133 | 100.0% |
| `System` | 50 | 40 | 40 | 10 | 0 | 80.0% |
| `Debug` | 13 | 11 | 11 | 2 | 0 | 84.6% |
| `Plugins` | 3 | 4 | 3 | 0 | 1 | 100.0% |
| `VideoOverlay` | 70 | 0 | 0 | 70 | 0 | 0.0% |
| `AsyncTrigger` | 6 | 0 | 0 | 6 | 0 | 0.0% |
| `MenuItem` | 22 | 0 | 0 | 22 | 0 | 0.0% |
| `Storages` | 13 | 15 | 11 | 2 | 4 | 84.6% |
| `Scripts` | 11 | 11 | 11 | 0 | 0 | 100.0% |
| `KAGParser` | 25 | 48 | 25 | 0 | 23 | 100.0% |
| `WaveSoundBuffer` | 33 | 0 | 0 | 33 | 0 | 0.0% |

## Missing members by class

### `Layer` — 75 missing

`adjustGamma`, `affineBlend`, `affineCopy`, `affinePile`, `beginTransition`, `blendRect`, `bringToBack`, `colorRect`, `convertType`, `copy9Patch`, `copyRect`, `copyToBitmapFromMainImage`, `cursorX`, `cursorY`, `doBoxBlur`, `doGrayScale`, `drawGlyph`, `drawText`, `dump`, `flipLR`, `flipUD`, `focusNext`, `focusPrev`, `gaussianBlur`, `getMainPixel`, `independProvinceImage`, `light`, `loadImages`, `loadProvinceImage`, `moveBefore`, `moveBehind`, `onBeforeFocus`, `onBlur`, `onClick`, `onDoubleClick`, `onFocus`, `onHitTest`, `onKeyDown`, `onKeyPress`, `onKeyUp`, `onMouseDown`, `onMouseEnter`, `onMouseLeave`, `onMouseMove`, `onMouseUp`, `onMouseWheel`, `onMultiTouch`, `onNodeDisabled`, `onNodeEnabled`, `onPaint`, `onSearchNextFocusable`, `onSearchPrevFocusable`, `onTouchDown`, `onTouchMove`, `onTouchRotate`, `onTouchScaling`, `onTouchUp`, `onTransitionCompleted`, `operateAffine`, `operateRect`, `operateStretch`, `pileRect`, `piledCopy`, `releaseCapture`, `releaseTouchCapture`, `removeMode`, `saveLayerImage`, `setAttentionPos`, `setCursorPos`, `setMainPixel`, `setMode`, `stopTransition`, `stretchBlend`, `stretchCopy`, `stretchPile`

### `Bitmap` — 5 missing

`getSaveOption`, `load`, `loadAsync`, `loadHeader`, `save`

### `Window` — 40 missing

`HWND`, `borderStyle`, `displayOrientation`, `displayRotate`, `drawDevice`, `findFullScreenCandidates`, `getMouseVelocity`, `getTouchPoint`, `getTouchVelocity`, `layerTreeOwnerInterface`, `onActivate`, `onClick`, `onCloseQuery`, `onDeactivate`, `onDisplayRotate`, `onDoubleClick`, `onFileDrop`, `onKeyDown`, `onKeyPress`, `onKeyUp`, `onMouseDown`, `onMouseEnter`, `onMouseLeave`, `onMouseMove`, `onMouseUp`, `onMouseWheel`, `onMultiTouch`, `onPopupHide`, `onResize`, `onTouchDown`, `onTouchMove`, `onTouchRotate`, `onTouchScaling`, `onTouchUp`, `registerMessageReceiver`, `resetMouseVelocity`, `stayOnTop`, `touchPointCount`, `touchRotateThreshold`, `touchScaleThreshold`

### `Font` — 13 missing

`defaultFaceName`, `doUserSelect`, `getEscHeightX`, `getEscHeightY`, `getEscWidthX`, `getEscWidthY`, `getGlyphDrawRect`, `getList`, `getTextHeight`, `getTextWidth`, `mapPrerenderedFont`, `rasterizer`, `unmapPrerenderedFont`

### `System` — 10 missing

`appDataPath`, `clearGraphicCache`, `desktopHeight`, `desktopLeft`, `desktopTop`, `desktopWidth`, `getKeyState`, `shellExecute`, `system`, `touchDevice`

### `Debug` — 2 missing

`console`, `controller`

### `VideoOverlay` — 70 missing

`VideoOverlay`, `audioBalance`, `audioVolume`, `brightness`, `brightnessDefaultValue`, `brightnessRangeMax`, `brightnessRangeMin`, `brightnessStepSize`, `cancelPeriodEvent`, `cancelSegmentLoop`, `close`, `contrast`, `contrastDefaultValue`, `contrastRangeMax`, `contrastRangeMin`, `contrastStepSize`, `enabledAudioStream`, `enabledVideoStream`, `fps`, `frame`, `height`, `hue`, `hueDefaultValue`, `hueRangeMax`, `hueRangeMin`, `hueStepSize`, `layer1`, `layer2`, `left`, `loop`, `mixingMovieAlpha`, `mixingMovieBGColor`, `mode`, `numberOfAudioStream`, `numberOfFrame`, `numberOfVideoStream`, `onCallbackCommand`, `onFrameUpdate`, `onPeriod`, `onStatusChanged`, `open`, `originalHeight`, `originalWidth`, `pause`, `periodEventFrame`, `play`, `playRate`, `position`, `prepare`, `resetMixingLayer`, `rewind`, `saturation`, `saturationDefaultValue`, `saturationRangeMax`, `saturationRangeMin`, `saturationStepSize`, `segmentLoopEndFrame`, `segmentLoopStartFrame`, `selectAudioStream`, `setBounds`, `setMixingLayer`, `setPeriodEvent`, `setPos`, `setSegmentLoop`, `setSize`, `stop`, `top`, `totalTime`, `visible`, `width`

### `AsyncTrigger` — 6 missing

`AsyncTrigger`, `cached`, `cancel`, `mode`, `onFire`, `trigger`

### `MenuItem` — 22 missing

`HMENU`, `MenuItem`, `add`, `caption`, `checked`, `children`, `enabled`, `fireClick`, `group`, `index`, `insert`, `keycodeToText`, `onClick`, `parent`, `popup`, `radio`, `remove`, `root`, `shortcut`, `textToKeycode`, `visible`, `window`

### `Storages` — 2 missing

`searchCD`, `selectFile`

### `WaveSoundBuffer` — 33 missing

`WaveSoundBuffer`, `bits`, `channels`, `fade`, `filters`, `flags`, `freeDirectSound`, `frequency`, `getVisBuffer`, `globalFocusMode`, `globalVolume`, `labels`, `looping`, `onFadeCompleted`, `onLabel`, `onStatusChanged`, `open`, `pan`, `paused`, `play`, `posX`, `posY`, `posZ`, `position`, `samplePosition`, `setPos`, `status`, `stop`, `stopFade`, `totalTime`, `useVisBuffer`, `volume`, `volume2`

## Extra members by class

Rust registrations with no reference counterpart; usually intentional engine internals.

### `Layer` — 64 extra

`Bitmap`, `Font`, `Timer`, `Window`, `__bind`, `add`, `angle`, `bold`, `buffer`, `bufferForWrite`, `bufferPitch`, `capacity`, `caption`, `changeScreenMode`, `close`, `color`, `copyFrom`, `count`, `enableTouch`, `faceIsFileName`, `focusedLayer`, `fullScreen`, `getPixel`, `hideMouseCursor`, `hintDelay`, `id`, `independ`, `innerHeight`, `innerWidth`, `interval`, `italic`, `loading`, `mainWindow`, `maxHeight`, `maxWidth`, `minHeight`, `minWidth`, `mode`, `mouseCursorState`, `moveToFront`, `nativeId`, `onLoaded`, `onTimer`, `postInputEvent`, `primaryLayer`, `remove`, `removeMaskRegion`, `setBitmap`, `setImage`, `setInnerSize`, `setMaskRegion`, `setMaxSize`, `setMinSize`, `setParentId`, `setPixel`, `setZoom`, `showModal`, `strikeout`, `trapKey`, `underline`, `useMouseKey`, `waitVSync`, `zoomDenom`, `zoomNumer`

### `Bitmap` — 124 extra

`Font`, `Layer`, `Timer`, `Window`, `__bind`, `absolute`, `absoluteOrderMode`, `add`, `angle`, `assignImages`, `attentionLeft`, `attentionTop`, `bold`, `bringToFront`, `cached`, `callOnPaint`, `capacity`, `caption`, `changeScreenMode`, `children`, `clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `close`, `color`, `copyFromBitmapToMainImage`, `count`, `cursor`, `enableTouch`, `enabled`, `face`, `faceIsFileName`, `fillRect`, `focus`, `focusable`, `focused`, `focusedLayer`, `font`, `fullScreen`, `getLayerAt`, `getProvincePixel`, `hasImage`, `hideMouseCursor`, `hint`, `hintDelay`, `hitThreshold`, `hitType`, `holdAlpha`, `id`, `ignoreHintSensing`, `imageHeight`, `imageLeft`, `imageModified`, `imageTop`, `imageWidth`, `imeMode`, `independMainImage`, `innerHeight`, `innerWidth`, `interval`, `isPrimary`, `italic`, `joinFocusChain`, `left`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `mainWindow`, `maxHeight`, `maxWidth`, `minHeight`, `minWidth`, `mode`, `mouseCursorState`, `moveToFront`, `name`, `nativeId`, `neutralColor`, `nextFocusable`, `nodeEnabled`, `nodeFocusable`, `nodeVisible`, `onTimer`, `opacity`, `order`, `parent`, `postInputEvent`, `prevFocusable`, `primaryLayer`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`, `remove`, `removeMaskRegion`, `setBitmap`, `setClip`, `setImage`, `setImagePos`, `setImageSize`, `setInnerSize`, `setMaskRegion`, `setMaxSize`, `setMinSize`, `setParentId`, `setPos`, `setProvincePixel`, `setSizeToImageSize`, `setZoom`, `showModal`, `showParentHint`, `strikeout`, `top`, `trapKey`, `type`, `underline`, `update`, `useAttention`, `useMouseKey`, `visible`, `waitVSync`, `window`, `zoomDenom`, `zoomNumer`

### `Window` — 96 extra

`Bitmap`, `Font`, `Layer`, `Timer`, `__bind`, `absolute`, `absoluteOrderMode`, `angle`, `assignImages`, `attentionLeft`, `attentionTop`, `bold`, `buffer`, `bufferForWrite`, `bufferPitch`, `cached`, `callOnPaint`, `capacity`, `changeScreenMode`, `children`, `clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `color`, `copyFrom`, `copyFromBitmapToMainImage`, `count`, `cursor`, `enabled`, `face`, `faceIsFileName`, `fillRect`, `focus`, `focused`, `font`, `getLayerAt`, `getMaskPixel`, `getPixel`, `getProvincePixel`, `hasImage`, `hint`, `hitThreshold`, `hitType`, `holdAlpha`, `id`, `ignoreHintSensing`, `imageHeight`, `imageLeft`, `imageModified`, `imageTop`, `imageWidth`, `independ`, `independMainImage`, `interval`, `isPrimary`, `italic`, `joinFocusChain`, `loading`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `mode`, `moveToFront`, `name`, `nativeId`, `neutralColor`, `nextFocusable`, `nodeEnabled`, `nodeFocusable`, `nodeVisible`, `onLoaded`, `onTimer`, `opacity`, `order`, `parent`, `prevFocusable`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`, `setBitmap`, `setClip`, `setImage`, `setImagePos`, `setImageSize`, `setMaskPixel`, `setParentId`, `setPixel`, `setProvincePixel`, `setSizeToImageSize`, `showParentHint`, `strikeout`, `type`, `underline`, `useAttention`, `window`

### `Font` — 130 extra

`Bitmap`, `Layer`, `Timer`, `Window`, `__bind`, `absolute`, `absoluteOrderMode`, `add`, `assignImages`, `attentionLeft`, `attentionTop`, `bringToFront`, `buffer`, `bufferForWrite`, `bufferPitch`, `cached`, `callOnPaint`, `capacity`, `caption`, `changeScreenMode`, `children`, `clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `close`, `color`, `copyFrom`, `copyFromBitmapToMainImage`, `count`, `cursor`, `enableTouch`, `enabled`, `fillRect`, `focus`, `focusable`, `focused`, `focusedLayer`, `font`, `fullScreen`, `getLayerAt`, `getMaskPixel`, `getPixel`, `getProvincePixel`, `hasImage`, `hideMouseCursor`, `hint`, `hintDelay`, `hitThreshold`, `hitType`, `holdAlpha`, `id`, `ignoreHintSensing`, `imageHeight`, `imageLeft`, `imageModified`, `imageTop`, `imageWidth`, `imeMode`, `independ`, `independMainImage`, `innerHeight`, `innerWidth`, `interval`, `isPrimary`, `joinFocusChain`, `left`, `loading`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `mainWindow`, `maxHeight`, `maxWidth`, `minHeight`, `minWidth`, `mode`, `mouseCursorState`, `moveToFront`, `name`, `nativeId`, `neutralColor`, `nextFocusable`, `nodeEnabled`, `nodeFocusable`, `nodeVisible`, `onLoaded`, `onTimer`, `opacity`, `order`, `parent`, `postInputEvent`, `prevFocusable`, `primaryLayer`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`, `remove`, `removeMaskRegion`, `setBitmap`, `setClip`, `setImage`, `setImagePos`, `setImageSize`, `setInnerSize`, `setMaskPixel`, `setMaskRegion`, `setMaxSize`, `setMinSize`, `setParentId`, `setPixel`, `setPos`, `setProvincePixel`, `setSize`, `setSizeToImageSize`, `setZoom`, `showModal`, `showParentHint`, `top`, `trapKey`, `type`, `update`, `useAttention`, `useMouseKey`, `visible`, `waitVSync`, `width`, `window`, `zoomDenom`, `zoomNumer`

### `Timer` — 133 extra

`Bitmap`, `Font`, `Layer`, `Window`, `__bind`, `absolute`, `absoluteOrderMode`, `add`, `angle`, `assignImages`, `attentionLeft`, `attentionTop`, `bold`, `bringToFront`, `buffer`, `bufferForWrite`, `bufferPitch`, `cached`, `callOnPaint`, `caption`, `changeScreenMode`, `children`, `clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `close`, `color`, `copyFrom`, `copyFromBitmapToMainImage`, `count`, `cursor`, `enableTouch`, `face`, `faceIsFileName`, `fillRect`, `focus`, `focusable`, `focused`, `focusedLayer`, `font`, `fullScreen`, `getLayerAt`, `getMaskPixel`, `getPixel`, `getProvincePixel`, `hasImage`, `height`, `hideMouseCursor`, `hint`, `hintDelay`, `hitThreshold`, `hitType`, `holdAlpha`, `id`, `ignoreHintSensing`, `imageHeight`, `imageLeft`, `imageModified`, `imageTop`, `imageWidth`, `imeMode`, `independ`, `independMainImage`, `innerHeight`, `innerWidth`, `isPrimary`, `italic`, `joinFocusChain`, `left`, `loading`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `mainWindow`, `maxHeight`, `maxWidth`, `minHeight`, `minWidth`, `mouseCursorState`, `moveToFront`, `name`, `nativeId`, `neutralColor`, `nextFocusable`, `nodeEnabled`, `nodeFocusable`, `nodeVisible`, `onLoaded`, `opacity`, `order`, `parent`, `postInputEvent`, `prevFocusable`, `primaryLayer`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`, `remove`, `removeMaskRegion`, `setBitmap`, `setClip`, `setImage`, `setImagePos`, `setImageSize`, `setInnerSize`, `setMaskPixel`, `setMaskRegion`, `setMaxSize`, `setMinSize`, `setParentId`, `setPixel`, `setPos`, `setProvincePixel`, `setSize`, `setSizeToImageSize`, `setZoom`, `showModal`, `showParentHint`, `strikeout`, `top`, `trapKey`, `type`, `underline`, `update`, `useAttention`, `useMouseKey`, `visible`, `waitVSync`, `width`, `window`, `zoomDenom`, `zoomNumer`

### `Plugins` — 1 extra

`Plugins`

### `Storages` — 4 extra

`Storages`, `fstat`, `getFileList`, `stat`

### `KAGParser` — 23 extra

`getCallStackDepth`, `getCurLabel`, `getCurLine`, `getCurLineStr`, `getCurPos`, `getCurStorage`, `getDebugLevel`, `getIgnoreCR`, `getMP`, `getMacroParams`, `getMacros`, `getMultiLineTagEnabled`, `getParamMacros`, `getProcessSpecialTags`, `multiLineTagEnabled`, `paramMacros`, `setCurStorage`, `setDebugLevel`, `setIgnoreCR`, `setMacros`, `setMultiLineTagEnabled`, `setParamMacros`, `setProcessSpecialTags`

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
| `SoundBuffer` | 0 | `crates/compat/src/natives/sound.rs#SoundBuffer` |
| `SoundChannel` | 0 | `crates/compat/src/natives/sound.rs#SoundChannel` |
| `GdiPlusAppearance` | 0 | `crates/compat/src/natives/visual.rs#GdiPlusAppearance` |
| `ChainItemBase` | 0 | `crates/compat/src/natives/trans.rs` |
| `Trans` | 0 | `crates/compat/src/natives/trans.rs` |

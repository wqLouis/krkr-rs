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
| matched | 3 |
| **missing** | **524** |
| extra (Rust only) | 1 |
| coverage | **0.6%** |

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Layer` | 150 | 0 | 0 | 150 | 0 | 0.0% |
| `Bitmap` | 20 | 0 | 0 | 20 | 0 | 0.0% |
| `Window` | 83 | 0 | 0 | 83 | 0 | 0.0% |
| `Font` | 22 | 0 | 0 | 22 | 0 | 0.0% |
| `Timer` | 6 | 0 | 0 | 6 | 0 | 0.0% |
| `System` | 50 | 0 | 0 | 50 | 0 | 0.0% |
| `Debug` | 13 | 0 | 0 | 13 | 0 | 0.0% |
| `Plugins` | 3 | 4 | 3 | 0 | 1 | 100.0% |
| `VideoOverlay` | 70 | 0 | 0 | 70 | 0 | 0.0% |
| `AsyncTrigger` | 6 | 0 | 0 | 6 | 0 | 0.0% |
| `MenuItem` | 22 | 0 | 0 | 22 | 0 | 0.0% |
| `Storages` | 13 | 0 | 0 | 13 | 0 | 0.0% |
| `Scripts` | 11 | 0 | 0 | 11 | 0 | 0.0% |
| `KAGParser` | 25 | 0 | 0 | 25 | 0 | 0.0% |
| `WaveSoundBuffer` | 33 | 0 | 0 | 33 | 0 | 0.0% |

## Missing members by class

### `Layer` — 150 missing

`Layer`, `absolute`, `absoluteOrderMode`, `adjustGamma`, `affineBlend`, `affineCopy`, `affinePile`, `assignImages`, `attentionLeft`, `attentionTop`, `beginTransition`, `blendRect`, `bringToBack`, `bringToFront`, `cached`, `callOnPaint`, `children`, `clipHeight`, `clipLeft`, `clipTop`, `clipWidth`, `colorRect`, `convertType`, `copy9Patch`, `copyFromBitmapToMainImage`, `copyRect`, `copyToBitmapFromMainImage`, `cursor`, `cursorX`, `cursorY`, `doBoxBlur`, `doGrayScale`, `drawGlyph`, `drawText`, `dump`, `enabled`, `face`, `fillRect`, `flipLR`, `flipUD`, `focus`, `focusNext`, `focusPrev`, `focusable`, `focused`, `font`, `gaussianBlur`, `getLayerAt`, `getMainPixel`, `getMaskPixel`, `getProvincePixel`, `hasImage`, `height`, `hint`, `hitThreshold`, `hitType`, `holdAlpha`, `ignoreHintSensing`, `imageHeight`, `imageLeft`, `imageModified`, `imageTop`, `imageWidth`, `imeMode`, `independMainImage`, `independProvinceImage`, `isPrimary`, `joinFocusChain`, `left`, `light`, `loadImages`, `loadProvinceImage`, `mainImageBuffer`, `mainImageBufferForWrite`, `mainImageBufferPitch`, `moveBefore`, `moveBehind`, `name`, `neutralColor`, `nextFocusable`, `nodeEnabled`, `nodeFocusable`, `nodeVisible`, `onBeforeFocus`, `onBlur`, `onClick`, `onDoubleClick`, `onFocus`, `onHitTest`, `onKeyDown`, `onKeyPress`, `onKeyUp`, `onMouseDown`, `onMouseEnter`, `onMouseLeave`, `onMouseMove`, `onMouseUp`, `onMouseWheel`, `onMultiTouch`, `onNodeDisabled`, `onNodeEnabled`, `onPaint`, `onSearchNextFocusable`, `onSearchPrevFocusable`, `onTouchDown`, `onTouchMove`, `onTouchRotate`, `onTouchScaling`, `onTouchUp`, `onTransitionCompleted`, `opacity`, `operateAffine`, `operateRect`, `operateStretch`, `order`, `parent`, `pileRect`, `piledCopy`, `prevFocusable`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch`, `releaseCapture`, `releaseTouchCapture`, `removeMode`, `saveLayerImage`, `setAttentionPos`, `setClip`, `setCursorPos`, `setImagePos`, `setImageSize`, `setMainPixel`, `setMaskPixel`, `setMode`, `setPos`, `setProvincePixel`, `setSize`, `setSizeToImageSize`, `showParentHint`, `stopTransition`, `stretchBlend`, `stretchCopy`, `stretchPile`, `top`, `type`, `update`, `useAttention`, `visible`, `width`, `window`

### `Bitmap` — 20 missing

`Bitmap`, `buffer`, `bufferForWrite`, `bufferPitch`, `copyFrom`, `getMaskPixel`, `getPixel`, `getSaveOption`, `height`, `independ`, `load`, `loadAsync`, `loadHeader`, `loading`, `onLoaded`, `save`, `setMaskPixel`, `setPixel`, `setSize`, `width`

### `Window` — 83 missing

`HWND`, `Window`, `add`, `borderStyle`, `bringToFront`, `caption`, `close`, `displayOrientation`, `displayRotate`, `drawDevice`, `enableTouch`, `findFullScreenCandidates`, `focusable`, `focusedLayer`, `fullScreen`, `getMouseVelocity`, `getTouchPoint`, `getTouchVelocity`, `height`, `hideMouseCursor`, `hintDelay`, `imeMode`, `innerHeight`, `innerWidth`, `layerTreeOwnerInterface`, `left`, `mainWindow`, `maxHeight`, `maxWidth`, `minHeight`, `minWidth`, `mouseCursorState`, `onActivate`, `onClick`, `onCloseQuery`, `onDeactivate`, `onDisplayRotate`, `onDoubleClick`, `onFileDrop`, `onKeyDown`, `onKeyPress`, `onKeyUp`, `onMouseDown`, `onMouseEnter`, `onMouseLeave`, `onMouseMove`, `onMouseUp`, `onMouseWheel`, `onMultiTouch`, `onPopupHide`, `onResize`, `onTouchDown`, `onTouchMove`, `onTouchRotate`, `onTouchScaling`, `onTouchUp`, `postInputEvent`, `primaryLayer`, `registerMessageReceiver`, `remove`, `removeMaskRegion`, `resetMouseVelocity`, `setInnerSize`, `setMaskRegion`, `setMaxSize`, `setMinSize`, `setPos`, `setSize`, `setZoom`, `showModal`, `stayOnTop`, `top`, `touchPointCount`, `touchRotateThreshold`, `touchScaleThreshold`, `trapKey`, `update`, `useMouseKey`, `visible`, `waitVSync`, `width`, `zoomDenom`, `zoomNumer`

### `Font` — 22 missing

`Font`, `angle`, `bold`, `defaultFaceName`, `doUserSelect`, `face`, `faceIsFileName`, `getEscHeightX`, `getEscHeightY`, `getEscWidthX`, `getEscWidthY`, `getGlyphDrawRect`, `getList`, `getTextHeight`, `getTextWidth`, `height`, `italic`, `mapPrerenderedFont`, `rasterizer`, `strikeout`, `underline`, `unmapPrerenderedFont`

### `Timer` — 6 missing

`Timer`, `capacity`, `enabled`, `interval`, `mode`, `onTimer`

### `System` — 50 missing

`System`, `addContinuousHandler`, `appDataPath`, `assignMessage`, `clearGraphicCache`, `createAppLock`, `createUUID`, `dataPath`, `desktopHeight`, `desktopLeft`, `desktopTop`, `desktopWidth`, `doCompact`, `drawThreadNum`, `dumpHeap`, `eventDisabled`, `exeBits`, `exeName`, `exePath`, `exit`, `exitOnNoWindowStartup`, `exitOnWindowClose`, `getArgument`, `getKeyState`, `getTickCount`, `graphicCacheLimit`, `inform`, `inputString`, `nullpo`, `osBits`, `osName`, `personalPath`, `platformName`, `processorNum`, `readRegValue`, `removeContinuousHandler`, `savedGamesPath`, `screenHeight`, `screenWidth`, `setArgument`, `shellExecute`, `showVersion`, `system`, `terminate`, `title`, `toActualColor`, `touchDevice`, `touchImages`, `versionInformation`, `versionString`

### `Debug` — 13 missing

`Debug`, `addLoggingHandler`, `clearLogFileOnError`, `console`, `controller`, `getLastLog`, `logAsError`, `logLocation`, `logToFileOnError`, `message`, `notice`, `removeLoggingHandler`, `startLogToFile`

### `VideoOverlay` — 70 missing

`VideoOverlay`, `audioBalance`, `audioVolume`, `brightness`, `brightnessDefaultValue`, `brightnessRangeMax`, `brightnessRangeMin`, `brightnessStepSize`, `cancelPeriodEvent`, `cancelSegmentLoop`, `close`, `contrast`, `contrastDefaultValue`, `contrastRangeMax`, `contrastRangeMin`, `contrastStepSize`, `enabledAudioStream`, `enabledVideoStream`, `fps`, `frame`, `height`, `hue`, `hueDefaultValue`, `hueRangeMax`, `hueRangeMin`, `hueStepSize`, `layer1`, `layer2`, `left`, `loop`, `mixingMovieAlpha`, `mixingMovieBGColor`, `mode`, `numberOfAudioStream`, `numberOfFrame`, `numberOfVideoStream`, `onCallbackCommand`, `onFrameUpdate`, `onPeriod`, `onStatusChanged`, `open`, `originalHeight`, `originalWidth`, `pause`, `periodEventFrame`, `play`, `playRate`, `position`, `prepare`, `resetMixingLayer`, `rewind`, `saturation`, `saturationDefaultValue`, `saturationRangeMax`, `saturationRangeMin`, `saturationStepSize`, `segmentLoopEndFrame`, `segmentLoopStartFrame`, `selectAudioStream`, `setBounds`, `setMixingLayer`, `setPeriodEvent`, `setPos`, `setSegmentLoop`, `setSize`, `stop`, `top`, `totalTime`, `visible`, `width`

### `AsyncTrigger` — 6 missing

`AsyncTrigger`, `cached`, `cancel`, `mode`, `onFire`, `trigger`

### `MenuItem` — 22 missing

`HMENU`, `MenuItem`, `add`, `caption`, `checked`, `children`, `enabled`, `fireClick`, `group`, `index`, `insert`, `keycodeToText`, `onClick`, `parent`, `popup`, `radio`, `remove`, `root`, `shortcut`, `textToKeycode`, `visible`, `window`

### `Storages` — 13 missing

`addAutoPath`, `chopStorageExt`, `clearArchiveCache`, `extractStorageExt`, `extractStorageName`, `extractStoragePath`, `getFullPath`, `getLocalName`, `getPlacedPath`, `isExistentStorage`, `removeAutoPath`, `searchCD`, `selectFile`

### `Scripts` — 11 missing

`Scripts`, `compileStorage`, `dump`, `eval`, `evalStorage`, `exec`, `execStorage`, `getClassNames`, `getTraceString`, `setCallMissing`, `textEncoding`

### `KAGParser` — 25 missing

`KAGParser`, `assign`, `callLabel`, `callStackDepth`, `clear`, `clearCallStack`, `curLabel`, `curLine`, `curLineStr`, `curPos`, `curStorage`, `debugLevel`, `getNextTag`, `goToLabel`, `ignoreCR`, `interrupt`, `loadScenario`, `macroParams`, `macros`, `mp`, `popMacroArgs`, `processSpecialTags`, `resetInterrupt`, `restore`, `store`

### `WaveSoundBuffer` — 33 missing

`WaveSoundBuffer`, `bits`, `channels`, `fade`, `filters`, `flags`, `freeDirectSound`, `frequency`, `getVisBuffer`, `globalFocusMode`, `globalVolume`, `labels`, `looping`, `onFadeCompleted`, `onLabel`, `onStatusChanged`, `open`, `pan`, `paused`, `play`, `posX`, `posY`, `posZ`, `position`, `samplePosition`, `setPos`, `status`, `stop`, `stopFade`, `totalTime`, `useVisBuffer`, `volume`, `volume2`

## Extra members by class

Rust registrations with no reference counterpart; usually intentional engine internals.

### `Plugins` — 1 extra

`Plugins`

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

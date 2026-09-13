# Stub / no-op / unimplemented audit — `krkr-rs` vs. the test game

**Game:** `/mnt/DATA/Games/Others/test` (`data.xp3` 23,572 entries + `patch.xp3` 174 entries).
**Date basis:** workspace as of the current `docs/` tree.
**Builds used:** DEV only (`./target/debug/krkr-rs`, `./target/debug/attention-harness`).
**Read-only audit:** no code was changed; this file is the only write.

## 1. Scope and method

1. Grepped `crates/{tvp-visual,tvp-natives,tvp-sound,tvp-scripts,tvp-kagparser,tvp-storages,tjs2-sys,render}`
   for `stub`, `noop`, `no_op`, `set_void_out`-only callbacks, `TODO/FIXME`,
   `unimplemented!`/`todo!`, "not implemented"/"not supported", empty native
   classes, and pending-error macros.
2. Extracted the real scripts and searched every stub name for concrete call
   sites (file:line) in `system/*.tjs`, `k2compat/*.tjs`, `patch.tjs`, plugin
   scripts, and every scenario `.ks` in `data.xp3` **and** `patch.xp3`.
3. Cross-checked `Plugins.link(...)` requests against the built-in emulations.
4. Cross-checked native class surfaces (`System`, `Storages`, `Menu`, `SaveStruct`,
   `CSVParser`, `VideoOverlay`, `LayerExDraw`, `WindowEx`, …) for real / partial / stub.
5. Ran the DEV headless loader and the attention harness to confirm what the
   currently-reachable startup path exercises.

**Runtime coverage caveat.** Headless mode dumps the first scene and exits;
`attention-harness` drives the VM through the logo→title milestone. Driving the
ADV scenario past the first `hitret` needs synthetic clicks, which is outside a
read-only audit. Therefore ADV/album/config findings below are established from
**static call-site evidence**, not from a fully-played run. No TJS exception is
logged on the startup/logo/title path (`RUST_LOG=warn,info ./target/debug/krkr-rs
run /mnt/DATA/Games/Others/test --headless` and
`./target/debug/attention-harness … 20` both end cleanly).

## 2. Script extraction verification

The pre-existing tree `/tmp/gamescripts/system/*.tjs` is **complete for
`data.xp3`'s `system/` scripts**: 36 `.tjs` files (the two `*.tjs.dec` files are
stale artifacts, not archive entries). It was missing two non-`.tjs` payloads
(`system/chardata.csv`, `system/movie.ks`) and the **six `patch.xp3` overrides**,
which are the versions actually loaded because `patch.xp3` is mounted last:

| override | loaded instead of |
|---|---|
| `advscreen.tjs` | `system/advscreen.tjs` |
| `album.tjs` | `system/album.tjs` |
| `configwindow.tjs` | `system/configwindow.tjs` |
| `gamescenemanager.tjs` | `system/gamescenemanager.tjs` |
| `systemwindow.tjs` | `system/systemwindow.tjs` |
| `title.tjs` | `system/title.tjs` |

For this audit a complete tree was re-extracted to `/tmp/audit_scripts/`
(`system/` = data.xp3, `*.tjs.patch` = patch.xp3, plus `k2compat/`, `startup.tjs`,
`begin.tjs`, `patch.tjs`, `system_data/chardata.csv`, `system_data/../movie.ks`,
all 84 data + 82 patch scenarios). Findings use the patch-override scripts where
one exists.

## 3. Plugin registration matrix (`Plugins.link`)

Requests at `system/initialize.tjs:127-137`, `system/sound.tjs:4`,
`k2compat/win32dialog.tjs:1`.

| Plugin (requested) | Request site | krkr-rs emulation | Verdict |
|---|---|---|---|
| `extrans.dll` | initialize.tjs:127 | `Trans` class registered but **no-op**; `Layer.beginTransition` is unrelated | **empty shell** (not called by this game) |
| `csvParser.dll` | initialize.tjs:128 | `CSVParser` real in tvp-storages (`initStorage`, `getNextLine`) | real |
| `layerExDraw.dll` | initialize.tjs:129 | *No class registered*: the plugin's methods are expected on `Layer`. `doBoxBlur` real; `doGrayScale`/`adjustGamma`/`flipLR`/`flipUD`/`light`/`operate*`/`stretch*`/`pile*`/`affine*` registered as **no-ops**; `colorRect`/`colorize`/`noise`/`doDropShadow`/`doBlurLight`/`tileRect`/`fillOperateRect` **not registered at all** | partial |
| `fstat.dll` | initialize.tjs:130 | `Storages.stat`/`fstat` real; `Storages.open`/`searchCD`/`selectFile` raise "not implemented" | partial |
| `windowEx.dll` | initialize.tjs:132 — **guarded by `@if(!kirikiriz)`**, and `startup.tjs:1` sets `@set(kirikiriz=1)`, so **never linked** | `System.getDisplayMonitors`/`getMonitorInfo`/`desktop*` still real; `Window` chrome members are stubs | N/A (not requested at runtime) |
| `KAGParserEx.dll` | initialize.tjs:135 | Real `KAGParser` instance class with properties (`ignoreCR`, `processSpecialTags`, `interrupt`), dict-returning `getNextTag`, macros; `ScController`/`AnimationSequenceController` subclass it directly | real |
| `getSample.dll` | initialize.tjs:137 — `@if(__DEBUGMODE__)` | ignored (debug only) | N/A |
| `wuvorbis.dll` | sound.tjs:4 | Real `WaveSoundBuffer` + Ogg Vorbis/Opus decode | real |
| `menu.dll` | `k2compat.tjs:237` delay-load | `MenuItem` native partial; `Window.menu` exists; used only by k2compat debug shortcuts | partial (debug path) |
| `win32dialog.dll` | `k2compat/win32dialog.tjs:1` — only if `WIN32Dialog` undefined; `startup.tjs` defines it, so **not linked** | `WIN32GenericDialogEX`/`TextContentModelessDialog` no-op bases anyway | N/A |
| `KAGParser.dll` | `k2compat.tjs:246` delay-load (only if `KAGParser` undefined; it is defined/linked) | n/a | N/A |

## 4. Prioritized stub table

File paths are relative to the repo root for code, and to
`/tmp/audit_scripts/` for game scripts. `data.xp3` line numbers are identical in
`patch.xp3` unless the file is one of the six overrides above.

### 4.1 Must fix to play

| # | Component | What is stubbed | Game call sites (file:line) | Impact if fixed | Effort | Notes |
|---|---|---|---|---|---|---|
| M1 | **`VideoOverlay` — movies** (`crates/tvp-natives/src/video_overlay.rs`, whole class; `vo_noop_method` at :324, registration :514; no `onStatusChanged`/`onPeriod`/`onFrameUpdate` events, `open`/`play` no-ops, `originalWidth/Height` always 0) | No decoding, no frame→layer, no status/period event dispatch | `system/movie.tjs:2` (`MovieLayer extends VideoOverlay`), `:6` `super.VideoOverlay`, `:36` `open`, `:43` `numberOfAudioStream`, `:69` `onStatusChanged`; `system/enveffect.tjs:648` `new VideoOverlay(win)`, `:650-651` binds `onStatusChanged`/`onPeriod`, `:698` `open`, `:742` `play`; `system/advscreen.tjs:6587` `PlayMovie`, `:1346-1350` handles `Movie.onStatusChanged`; `system/title.tjs:910` `PlayMovie("movie", …)`; **`scenario_patch/01_01.ks:3051` `@PlayMovie file=atropos`**; `scenario_data/system/movie.ks:5` | **Blocks chapter 1→2.** `@PlayMovie` creates a full-screen `ltOpaque` **black** layer at `LAYER_MOVIE` (`system/movie.tjs:8-25`). Because the stub never emits the `"stop"` status, `onStatusChanged`→`advscreen.onStopMovie` (`:1346-1350`, `:6591`) never runs, so the black layer is never removed and `_fMovie` stays true. The next scenario (`@change target=02_01`, `01_01.ks:3052`) then plays *under* the black overlay until the user right-clicks (`advscreen.tjs:464`). Also kills all `@veffect` video effects (8 uses in `01_01.ks`) and the title movie replay (`title.tjs:910`). | **L** (real MPEG/WMV/FLV/SWF decode + frame upload + audio + status/period events; any external decoder must build with the `zig c++` toolchain) | The file actually exists: `data.xp3: effect/atropos.mpg`; `AddAutoPath("data.xp3>effect/")` (`initialize.tjs:64`) makes `Storages.isExistentStorage("atropos.mpg")` true, so `open()` really is called. This is the single hard blocker found. |

### 4.2 Visible but non-blocking

All of these either no-op silently or only affect optional/debug UI. They do not
prevent advancing the story; they degrade the picture, sound or a secondary screen.

| # | Component | What is stubbed | Game call sites (file:line) | Impact if fixed | Effort | Notes |
|---|---|---|---|---|---|---|
| V1 | **Layer affine / image ops** (`crates/tvp-visual/src/natives/layer.rs`, `layer_noop` :1999, `noop_stubs` list :2185) — `affineCopy/Pile/Blend`, `stretchCopy/Pile/Blend`, `pileRect`, `piledCopy`, `blendRect`, `operateRect/Stretch/Affine`, `light`, `adjustGamma`, `doGrayScale`, `flipLR/UD`, `clear`, `drawImage*`, `drawGlyph/String/Curve/Pie/Ellipse/Path`, `saveLayerImage`, `independ*Image`, `gaussianBlur`, `convertType`, `setCenter`, `setAffineOffset` | Pixel/affine compositing is not performed; the calls return void | `system/affinelayer.tjs:210,341,346,351,356,361,391,400,406,425,430,435,439,462,467,472,477,482,531,536,541,674`; `system/utility.tjs:214,216,222,226,227`; `system/advscreen.tjs:974,975,981,982,3530-3532,3675,7419,7420,7427`; `system/messageframe.tjs:1034,419,429,447`; `system/messagearea.tjs:1063`; `system/album.tjs:770,772`; `system/eyecatch.tjs:75-79`; `system/staffroll.tjs:357,550` | Rotation/zoom, crossfades, 4-quadrant `@flash`, `@blackout`/`@update` interpolation, gamma/gray/flip tone, character-face alpha mask, CG-gallery compose and save thumbnails render correctly. | **M–L** | `blur_bitmap`, `raster` and `blend_pixel` helpers already exist; the missing piece is a real affine/stretch blit and tone pipeline. `setCenter`/`setAffineOffset` only matter once `affineCopy` works. |
| V2 | **`Layer.beginTransition` interpolation** (`layer.rs:1963` queues only a next-poll `onTransitionCompleted`; `transition_poll` :1985) | Transition runs to completion instantly; no time-based interpolation | `system/sprite.tjs:286,290`; `system/activatelayer.tjs:237,241`; `system/advscreen.tjs:3353,3719,3735,3737,3739`; `system/advobject.tjs:295`; `system/staffroll.tjs:550` | Real crossfade / scroll / universal transitions instead of hard cuts. | **M** | Completion callbacks and `_isTransition` flags already work; only the visual ramp is missing. Every scene change / CG change uses this. |
| V3 | **`Layer.colorRect` missing** (not registered; `Layer` has `fillRect` only) | Method does not exist on the native `Layer`, so `_image.colorRect(...)` throws "member not found" | `system/utility.tjs:1752-1765` (`DrawFrame`); `system/album.tjs:451`; `system/messageframe.tjs:782,785`; `system/editlayer.tjs:96,105`; `system/selectitem.tjs:414-416,1200,1672-1674,1832,1839`; `system/advobject.tjs:266-268` | CG-gallery progress bars, save-comment caret, and debug overlays stop throwing. | **S** | `AffineLayer.colorRect` (`affinelayer.tjs:391`) forwards to `_image`; implement as a blend-aware fill. All direct callers found are in the album, comment editor, or `@if(__DEBUGMODE__)` blocks, so the **main story is not hit**. |
| V4 | **`Layer.colorize/noise/doDropShadow/doBlurLight/tileRect/fillOperateRect` missing** | Not registered at all (would throw if invoked) | Definitions only: `system/affinelayer.tjs:679,684,689,694,699,704` (no call sites found outside those wrappers) | Complete the `LayerExDraw` surface. | **S each** | Not currently reachable from any scenario tag or system path; listed here because the wrappers exist and a future caller will throw. |
| V5 | **`Layer.neutralColor` property missing** | Not registered; read by `AffineLayer.onPaint` | `system/window.tjs:58` sets it; `system/affinelayer.tjs:203` reads it (property wrapper at `:652-657`) | The zero-area affine fallback fills with the intended color instead of undefined/black. | **S** | Property assignment on a native instance silently creates a dynamic member, so this only matters for the native read path. |
| V6 | **`Font.mapPrerenderedFont` is a no-op** (`crates/tvp-visual/src/natives/font.rs:409-448`) | Never registers `.tft` bitmap fonts; vector fallback is used | `system/initialize.tjs:229` → `system/system.tjs:714-729` `PrerenderedFontInit()` → `mapPrerenderedFont("….tft")`; message areas are constructed with `usePrerenderedFont=true`: `system/messageframe.tjs:139,142`, `system/confirm.tjs:64,223`, `system/systemwindow.tjs:2115`, `system/configwindow.tjs:400`, `system/advobject.tjs:605` | Dialogue uses the game's intended `.tft` faces (スーラ/ハミング/…), pixel-accurate metrics. | **L** (need a `.tft` parser; the files are `data.xp3`+`patch.xp3`: `スーラ12.tft`, `ハミング30.tft`, …) | Layout stays self-consistent because `MessageArea` measures with the same `font` it draws with, so text is readable today — only the typeface/metrics differ from the original. |
| V7 | **`Window` OS-chrome stubs** (`crates/tvp-visual/src/natives/window.rs`): `setSize`/`setPos`/`setZoom` → `window_noop2` :457; `add`/`remove` → `window_add_remove_noop` :386; `bringToFront`/`update`/`hideMouseCursor` :428/:414/:442; `fullScreen` getter hard-coded 0 :734-742; `registerExEvent`/`changeScreenMode`/`addInputNotify` mapped to no-ops :530-560 | OS window size/position/zoom/fullscreen/stay-on-top and cursor hiding not applied | `system/window.tjs:50` `setInnerSize` (real), `:70,126,129` `changeScreenMode`, `:254` `setZoom`, `:83` `stayOnTop`; `system/configwindow.tjs:1092,1243-1248` reads `window.fullScreen`; `system/gamescenemanager.tjs:285` | Windowed/fullscreen toggle, stay-on-top, zoom actually affect the host window; config screen-mode UI reflects reality. | **M** | `MainWindow` overrides `changeScreenMode`/`addInputNotify` in script, so their native no-ops are shadowed. `fullScreen` always reporting false is the user-visible part. |
| V8 | **`System` stubs** (`crates/tvp-natives/src/system.rs`): `System.system` :213 returns 0, `readRegValue` :232 void, `dumpHeap` :379 void, `doCompact` no-op, `inform` :106 logs only, `eventDisabled` boolean only | No process launch/registry/heap/compaction/message-box | `initialize.tjs:13` `createAppLock` (real, returns 1), `:14,35` `inform`, `:32` `eventDisabled`; `gamescenemanager.tjs:93`, `advscreen.tjs:6099` `doCompact`; `advscreen.tjs:1198` related debug | Native message boxes / heap dumps. | **S** | None of the stubbed ones affect gameplay; `createAppLock` already returns "first instance wins". |
| V9 | **`Debug.addLoggingHandler`/`removeLoggingHandler` no-ops** (`crates/tvp-natives/src/debug.rs:240,258`) | Logging-handler list not implemented | `k2compat/k2compat.tjs:103,146` | Custom log routing. | **S** | Only the k2compat error/trace path. |
| V10 | **`Storages.open`/`searchCD`/`selectFile` raise "not implemented"** (`crates/tvp-storages/src/lib.rs:949-963`, macro at :885) | Pending natives | `system/advscreen.tjs:1198` `Storages.selectFile(param)` (F10/F11 debug editor picker); no call sites for `open`/`searchCD` | Debug editor file picker. | **S–M** | `selectFile` is only reachable from the debug text-editor shortcut; if it ever throws it is inside a debug-only branch. |
| V11 | **`Scripts.exec/eval/execStorage/evalStorage` ignore the context argument** (`crates/tvp-scripts/src/lib.rs:503,543,579,619`) | Re-entrant execution context dropped; logs a warning | No game call passes a context: `startup.tjs:1669-1681`, `system/system.tjs:257,429,715`, `system/album.tjs:103`, `system/staffroll.tjs:52` | Full reference semantics. | **S** | The game's uses all work (object/function results are returned via the retained-value ABI). |
| V12 | **Blend modes fall back to source-over** (`crates/render/src/blend.rs:81-90`) | `ltMultiplicative`, `ltBinder`, `ltAddAlpha`, screen/PS-HardLight/Dodge/etc. composite as normal alpha | `.type = ltMultiplicative` / `ltBinder` / `ltAddAlpha` assignments in `system/*.tjs` (a few each; e.g. `enveffect.tjs:649` sets `_screen.type = ltBinder`) | Correct exotic blends. | **M** | `ltOpaque`/`ltAdditive`/`ltSubtractive` are real GPU blend states. Low frequency in this title. |
| V13 | **GdiPlus hatch/AA approximations** (`crates/tvp-visual/src/natives/gdiplus.rs:49-53`, hatch approximated by a diagonal pattern) | Approximate hatch fill and antialiasing | `GdiPlus.HatchStyleDiagonalBrick` used once (DrawFrameCurve family) | Pixel-accurate hatch. | **S** | The rest of `GdiPlus.Appearance` (ordered `addBrush`/`addPen` + real `Layer.draw*`) is implemented. |
| V14 | **Sound `speed` stored only / `filters` returns a fresh empty array / `PhaseVocoder` empty class** (`crates/tvp-sound/src/wavesound.rs:41-47,910,929,987-1003`) | Playback-rate/pitch filtering no-op | `system/sound.tjs:195-198` (`createFilter:1` path), `:482,486` (`createFilter:1`); `system/advscreen.tjs:5716` `ChangeBgmSpeed` | Variable-speed BGM. | **M** | BGM always plays at 1.0×; no scenario passes `speed` (grep of all `.ks` found zero `speed=` tags), so this title never changes tempo. |
| V15 | **`Mouse.getClickCount` always 0** (`crates/tvp-input/src/mouse.rs:229,316`) | Double-click detection absent | **No `Mouse.*` call sites in any system/k2compat/scenario script** | Double-click gestures. | **S** | Not exercised. |

### 4.3 Not exercised by this game (safe to leave stubbed for this title)

| # | Component | Stub | Evidence it is unused |
|---|---|---|---|
| N1 | `extrans` `Trans` class (`crates/tvp-natives/src/extrans.rs`) | no-op construct/start | No `Trans` reference in any `.tjs`/`.ks`/`patch.tjs` (the modern surface is `Layer.beginTransition`). |
| N2 | `plugin_stubs` no-op bases (`plugin_stubs.rs:73-86`): `InputNotifyBase`, `WIN32GenericDialogEX`, `TextContentModelessDialog`, `SubMenu`, `SliderV` | Empty native classes | All five are **also defined as script classes** (`InputNotifyBase` `system/window.tjs:631`; `SubMenu` `system/selectitem.tjs:2433`; `SliderV` `:1007`; `WIN32GenericDialogEX` `k2compat/win32dialog.tjs:490`; `TextContentModelessDialog` `k2compat/k2compat_padcommon.tjs:6`) and the script definitions shadow the natives. Harmless, but the `SubMenu`/`SliderV` entries in the stub comment are mislabeled. |
| N3 | `Layer` pixel-probe/ordering no-ops: `setMainPixel`, `getMainPixel`, `setMaskPixel`, `getMaskPixel`, `setProvincePixel`, `getProvincePixel`, `loadProvinceImage`, `bringToBack`, `moveBefore`, `moveBehind`, `focusNext`, `focusPrev`, `getList`, `onHitTest`, `setAttentionPos`, `captureMouse`, `captureTouch` | no-ops | No call sites found (only `Layer.font`/`Font.getList` in the font-select dialog, which is itself option-triggered). |
| N4 | `Layer` curve/ellipse/pie/path/image draw no-ops: `drawCurve*`, `drawClosedCurve*`, `drawEllipse`, `drawPie`, `drawPath`, `drawImage*`, `drawString`, `drawGlyph`, `drawRectangles` | no-ops | No call sites. |
| N5 | `Layer.setFontStyle`, `setDefaultDrawTextParam`, `resetDrawTextParam`, `getDrawWidth` | no-ops | `setDefaultDrawTextParam`/`resetDrawTextParam` *are* called (`messageframe.tjs:419-506`, `systemwindow.tjs:2118-2120`) but `MessageArea` calls `Layer.drawText` with explicit args, and `getDrawWidth` is `FontGlyph.getDrawWidth` (a script method, `messagearea.tjs:1083`), not the Layer one. |
| N6 | `Scripts.dump` / `dumpStringHeap` | Not registered | No call sites. |
| N7 | `KAGParserEx` additions `getRawTag*`, `getTag`, `isTagAvailable`, `getParameter` | Not implemented | Not used by `ScController`/`AnimationSequenceController`; no `[iscript]` blocks anywhere. |
| N8 | `KAGParser` owner events (`onScenarioLoad`, `onLabel`, `onJump`, …) | Not fired | The game polls `getNextTag()` instead. |
| N9 | `LayerExMovie` | No class | Not referenced by any script. |
| N10 | `Storages.open` / `searchCD` | Pending | No call sites. |
| N11 | `System.system`, `readRegValue`, `dumpHeap`, `showVersion`, `nullpo` | Stubs | No call sites (only `createAppLock`/`doCompact`/`inform` are used). |

## 5. Native class surface matrix (the classes named in the task)

| Class | Registered? | State | Game use |
|---|---|---|---|
| `VideoOverlay` | yes (`tvp-natives/video_overlay.rs`) | **stub** (all no-op, no events) | `MovieLayer` (`movie.tjs`), `EnvEffectFilter` (`enveffect.tjs`), `@PlayMovie` |
| `Layer` (LayerExDraw surface) | yes (`tvp-visual/natives/layer.rs`) | **real core + large no-op surface + 7 missing methods** (`colorRect`, `colorize`, `noise`, `doDropShadow`, `doBlurLight`, `tileRect`, `fillOperateRect`) | ADV compositing, transitions, effects |
| `LayerExMovie` | no | n/a | not used |
| `Window` (WindowEx surface) | yes (`tvp-visual/natives/window.rs`) | **partial** (real `setInnerSize`, `primaryLayer`, add/remove-layer state; OS chrome/fullscreen no-ops) | `MainWindow` (`system/window.tjs`) |
| `Menu` / `MenuItem` / `Window.menu` | `MenuItem` yes (`tvp-natives/menu_item.rs`), `Window.menu` yes | **partial** (logical tree; `popup` no-op) | k2compat debug shortcuts only |
| `SaveStruct` (`Array.saveStruct`/`Dictionary.saveStruct`) | VM built-in + file streams (`tjs2-sys/cpp/streams.cpp`) | **real** (filesystem, UTF-16LE; `"z"` compression ignored) | `system/system.tjs:333,336,447,450`; `Scripts.evalStorage` load |
| `CSVParser` | yes (`tvp-storages/csv_parser.rs`) | **real** (`initStorage`, `getNextLine`) | `system/system.tjs:663`, `system/advobject.tjs:875` |
| `Storages` | yes (`tvp-storages/lib.rs`) | **real core** (`stat`/`fstat`/`getFileList`/`isExistentStorage`/`copyFile`/`deleteFile`/auto-path/…); `open`/`searchCD`/`selectFile` pending | save/load, asset existence checks |
| `System` | yes (`tvp-natives/system.rs`) | **mostly real** (`getKeyState` is wired from the input bridge; `getTickCount`, `getDisplayMonitors`, `getMonitorInfo`, `desktop*`, `shellExecute`, `exit`); `system`/`readRegValue`/`dumpHeap`/`doCompact`/`inform` stubs | used heavily |
| `Font` | yes (`tvp-visual/natives/font.rs`) | real `face/height/color/bold/italic/underline/strikeout/angle`, real `getTextWidth`/`getTextHeight`; **`mapPrerenderedFont` no-op**; `size/indent/bkcolor` are script-level dynamic members | dialogue text |
| `Timer`/`OnceCall` | yes (`tvp-visual/natives/timer.rs`) | real | scenario/logo timing |
| `WaveSoundBuffer`/`SoundChannel` | yes (`tvp-sound`) | real decode + status/fade events; `speed`/`filters`/`PhaseVocoder` stubs | BGM/SE/voice |
| `KAGParser` | yes (`tvp-kagparser`) | real `getNextTag` dicts, macros, `if`, jumps/calls, properties | `ScController`, `AnimationSequenceController` |

## 6. Top items — concise summary

1. **`VideoOverlay` movies are the only hard blocker found (M1).** The whole class
   is a no-op with no `onStatusChanged`/`onPeriod`; `@PlayMovie` (`01_01.ks:3051`,
   `effect/atropos.mpg` really exists) creates an opaque black `LAYER_MOVIE` layer
   that is never torn down, so the first chapter boundary leaves the next
   scenario hidden behind black until a right-click. Fixing it also restores the
   title movie and all 8 `@veffect` video effects.
2. **`Layer` image/affine ops are the biggest visual gap (V1/V2).** ~68 no-op
   methods (`affineCopy`, `stretchCopy`, `piledCopy`, `operate*`, `adjustGamma`,
   `doGrayScale`, `flipLR/UD`, `light`, `saveLayerImage`, …) plus an instant
   `beginTransition`. The game composes every transition, flash, tone and
   thumbnail through `AffineLayer` → these methods, so scenes cut hard and many
   effects are invisible. No crash, so non-blocking.
3. **Seven `Layer` methods are missing entirely, not even no-ops (V3/V4).**
   `colorRect` throws today in the CG gallery (`album.tjs:451`), the save-comment
   caret (`editlayer.tjs:96,105`) and debug overlays; `colorize`, `noise`,
   `doDropShadow`, `doBlurLight`, `tileRect`, `fillOperateRect` have wrappers in
   `affinelayer.tjs` but no callers. `colorRect` is a cheap `S` fix.
4. **`Font.mapPrerenderedFont` no-op (V6)** means the game's `.tft` fonts are
   ignored (`initialize.tjs:229`) even though every message area requests
   prerendered text; dialogue renders in a vector fallback. Large (`.tft` parser),
   non-blocking.
5. **Everything else is minor or unexercised:** `Window` fullscreen/chrome,
   `System`/`Debug` helpers, `Storages selectFile` (debug editor),
   `Scripts` context arg, sound `speed`/filters (no scenario uses `speed`),
   `Mouse.getClickCount` (no `Mouse.*` calls), `extrans Trans` (never
   referenced), and the no-op plugin bases that are shadowed by script classes.

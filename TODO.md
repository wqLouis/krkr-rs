# TODO — open work

**Branch:** `migration/middle-layer-ash` · **repo:** `/mnt/DATA/Document/code/krkr-rs`
**Real game:** `/mnt/DATA/Games/Others/test` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

Committed through `d602c98`. The gate is green; `scripts/plugin_parity_allow.txt` is
**empty** (plugin parity 100%, 0 missing, 0 extras). This file lists only what is
still open. History is read with `git log`.

---

## 0. Conventions

* **Subagents must not run `cargo`** (`build`/`check`/`clippy`/`test`/`run`) — parallel builds once exhausted a 16 GB tmpfs and broke the tree. They may use `rustfmt`/`cargo fmt`, run prebuilt artifacts (`target/debug/krkr`, `target/debug/deps/*`), and run the Python scripts. The main agent runs the gate.
* **Only the main agent runs `scripts/surface_lock.py --update`.**
* **Comments** (D31): describe the code as it is — no history, no "used to", no rationale, no decision numbers. Keep `// SAFETY:`, invariants, and reference citations **in tests**.
* **Tests** carry a `reference/cpp/...:line` citation above every behaviour assertion. An assertion without one is not evidence.
* **Printed/logged text must not use internal jargon** (`L1`–`L4`, decision numbers). Tests are exempt.
* **D32:** only the Android backend may be stubbed; L3 may not stub and may not approximate. `#[allow(dead_code)]` is banned (one exception: `crates/compat/tests/common/mod.rs`).

## 1. The gate (run in this order)

```bash
cargo fmt --all -- --check
bash scripts/check-stateless.sh                                   # D9 guard
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p platform --features platform-linux
python3 scripts/surface_lock.py --update                          # then review + commit the diff
python3 scripts/surface_lock.py
python3 scripts/native_parity.py
python3 scripts/plugin_parity.py --no-doc
```

Real-game oracle (expect no `WARN`/`ERROR`, `texture >= 1`):

```bash
target/debug/krkr --headless --frames 10000 --base-dir /mnt/DATA/Games/Others/test
```

## 2. Landed / in flight

1. **`Bitmap(name)` ignored a storage name** — landed (`572f2ce`). It was the direct
   cause of the invisible checkboxes, radio buttons and sliders: every KAG widget
   image is built as `new Bitmap("FRM_02A31")` (`configwindow.tjs:324`,
   `messageframe.tjs:1546`, `album.tjs:1936`). The constructor now matches the
   reference `Construct` (`BitmapIntf.cpp:19-43`).
2. **`Layer.beginTransition` — game blocker** — landed (`a63b9fb`). `Sprite.tjs:290`
   calls `super.beginTransition(name, withcildren, transsrc, options)`; the reference
   passes `withchildren` (default **true**, `LayerIntf.cpp:9819-9821`) to
   `StartTransition`, which requires the endpoint main images **only when
   `!withchildren`** (`:7860-7868`) and otherwise transitions the whole tree through
   the cache (`:7869-7876`).
3. **`Layer` has no default image** — see R1 below; the last big rendering bug.

## 3. KAG rendering audit — verified findings, not yet fixed

A full render/UI audit of the KAG system scripts (`kag-render-audit`, evidence in
`/tmp/kag-render-audit.md`). Root causes, most important first.

* **R1 (P0) — a constructed `Layer` has no image, and `setSize` does not grow one.**
  The reference ctor ends with `AllocateDefaultImage()` (`LayerIntf.cpp:400`), which
  assigns the shared 32×32 transparent-white bitmap and calls `ResetClip()`; `Rect`
  defaults to `0,0,32,32` (`:339-342`), so a fresh layer has `MainImage != nullptr`
  and `ClipRect = 0,0,32,32`. `setSize` → `InternalSetSize` → `ImageLayerSizeChanged`
  **grows** `MainImage` to the rect (`:2656-2673`). The port creates
  `bitmap: None, rect: 0,0,0,0` and `layer_set_size` never allocates
  (`scene.rs:437-438`, `layer/mod.rs:367-402`), so `hasImage`, `copyRect`,
  `stretchCopy`, `colorRect` and `Layer.clear` no-op. This is why slider tracks,
  album thumbnails, procedural sprites and several panels render blank. **This
  corrects `5c247bd`:** that revert quoted the ctor's pre-`AllocateDefaultImage`
  `ClipRect = 0` (`:375-378`) and missed `ResetClip()` at `:400`; the test
  `copy_rect_on_an_imageless_layer_is_a_silent_no_op` (`layer_behaviour.rs:1095`)
  encodes the wrong premise. `LayerExDraw.with_layer` allocating on demand
  (`layer_ex/draw.rs:601`) masks this for `draw*` only.
* **R3 (P1) — `fillRect` on an imageless layer paints the whole layer, last colour
  wins.** `layer/mod.rs:1143-1150` stores one `layer.fill_color` for the whole rect;
  the reference fills only `rect ∩ ClipRect` inside `MainImage` (`:4272-4320`). Five
  nested fills in `selectitem.tjs:1178` collapse to one.
* **R4 (P1) — prerendered fonts (`.tft`) are ignored.** 24 `font/*.tft` ship in
  `data.xp3` (24 more in `patch.xp3`); `system/system.tjs` maps them through
  `Font.mapPrerenderedFont`. The port records the name in a map nothing reads
  (`font.rs:651-683`) and never raises when the storage is missing, so the KAG
  `catch` never fires. Text is rasterized with the L1 fallback face instead of the
  shipped bitmap face — wrong glyphs and wrong `getTextWidth` metrics. **This is the
  prime suspect for the "some text does not render / white on white" report.**
* **R5 (P2)** `setImageSize` does not shrink the rect or clamp `ImageLeft/Top`
  (`:2623-2640`); `setImagePos` accepts out-of-range values (`:2506-2523`).
* **R6 (P2)** `Font.getTextWidth`/`getTextHeight` ignore `angle`
  (`LayerIntf.cpp:11716-11746` multiplies by `cos(angle·π/1800)`).
* **R7 (P2)** `System.stayOnTop` is not a registered member though
  `configwindow.tjs:1264` and `window.tjs:83` assign it.
* **R8 (P2)** `Layer.hasImage` reports false on a fresh layer where the reference
  reports true (`:2497`); it is R1's observable face.

## 4. Win32 compatibility: dialogs and popups

Decision: dialogs/popups belong to a **platform-level compatibility layer**, so the
same script gets a native-looking dialog on whichever OS runs it. They are **not** to
be faked in the compat layer.

* Linux: implement the dialog ourselves — either drawn by our own engine or hosted
  in GTK. Not decided; **left for later**.
* The non-dialog Win32 surface (registry, shell, system metrics, synthetic handles,
  message receivers) stays emulated in compat, and a typed error there is still a
  compatibility bug.

Consequence: the missing `WIN32Dialog` surface (`Header`, `Items`, `Bitmap`, `Blob`,
`SolidBrush`, `getOctetAddress`, `getStringAddress`, `getStringFromAddress`,
`initCommonControlsEx`, and the instance API; `natives/plugins/win32_dialog/mod.rs`)
stays open, as does the `System.getDisplayMonitors`/`getMonitorInfo` nested
`work`/`monitor` shape (`window_ex/system.rs:110-128`) that the shipped
`k2compat/deskinfo` and `k2compat/scrinfo` read.

## 5. Audit backlog — every divergence the three audits reported

Sources: `/tmp/kag-render-audit.md`, `/tmp/kag-compat-audit.md`, `/tmp/kag-flow-audit.md`.
`[x]` landed · `[~]` assigned to an agent · `[ ]` open.

### Rendering / layers (`kag-render-audit`)

* [~] **R1 (P0)** a constructed `Layer` must have the default 32×32 image,
  `Rect = 0,0,32,32` and `ClipRect = 0,0,32,32`, and `setSize` must grow
  `MainImage` (`LayerIntf.cpp:339-342,400,2381-2391,2656-2673`). Corrects `5c247bd`;
  the test `copy_rect_on_an_imageless_layer_is_a_silent_no_op` encodes the wrong
  premise.
* [x] **R2 (P0)** `beginTransition`'s `withchildren` (`:9819-9821,7860-7876`) —
  landed `a63b9fb`.
* [ ] **R3 (P1)** `fillRect` must fill only `rect ∩ ClipRect` inside `MainImage`
  (`:4272-4320`); the port-only `layer.fill_color` must go.
* [x] **R4 (P1)** prerendered `.tft` fonts — implemented. The format was
  established from `reference/cpp/core/visual/PrerenderedFont.{h,cpp}` **and** a
  real extracted `font/スーラ12.tft` (36-byte header, `ChIndex` sorted UTF-16,
  20-byte packed descriptors, version-0/1 RLE coverage, `min(b*4, 255)` upscale),
  not invented. `mapPrerenderedFont`/`unmapPrerenderedFont` now load and validate,
  a missing/malformed storage raises (`TVPCannotFindStorage`/
  `TVPPrerenderedFontMappingFailed`), and the draw and metric paths use the mapped
  glyphs. The reference only consults the map through a layer-bound font
  (`tTJSNI_Font::GetTextWidth` checks `if(Layer)`), which the port mirrors with
  `owns_state`. Known omission: the port's key is
  `(face, height, bold, italic, angle)` where the reference's `tTVPFont` also folds
  in underline/strikeout/fontfile — no game impact (`PRFont.tjs` has them false).
* [ ] **R5 (P2)** `setImageSize` must shrink the rect and clamp `ImageLeft/Top`
  (`:2623-2640`); `setImagePos` must reject out-of-range positions (`:2506-2523`).
* [x] **R6 (P2)** — **corrected by the implementing agent, and it was my error**:
  the reference does **not** rotate `getTextWidth`/`getTextHeight`.
  `GetTextWidthDirect` is an unrotated extent sum and `GetTextHeight` is
  `abs(Font.Height)` (`LayerIntf.cpp:11702-11731`); only `getEsc*` multiply by
  `cos/sin(angle·π/1800)` (`:11734-11746`), which the port already did. The real
  divergence in that range was `measure_text` returning the shaped block height
  instead of `abs(Font.Height)` — now fixed. Adding rotation to the plain getters
  would have been a new divergence.
* [x] **R7 (P2)** `System.stayOnTop` — **verified not a reference member** (only
  `Window.stayOnTop`, `WindowIntf.cpp:1608`); the script writes a dynamic member.
  Nothing to add.
* [ ] **R8 (P2)** `Layer.hasImage` reports false on a fresh layer where the reference
  reports true (`:2497`); it is R1's observable face.
* [ ] **R8b (P2)** `hasImage = true` must raise `TVPLayerCannotHaveImage` when the
  layer's type cannot carry an image (`LayerIntf.cpp:2489-2493`). The port collapses
  `ltBinder`/`ltEffect`/`ltFilter` — the only `CanHaveImage == false` types — into
  the generic `BlendMode::SourceOver`, and `engine::LayerState` has no
  `can_have_image` field, so this needs engine state plus the layer-type mapping.
  The observable parts of `SetHasImage` (`true` allocates, `false` deallocates, so
  destination-rect ops behave as the reference) are implemented and tested.
* [ ] **R9 (P1, new)** the **normal projection ignores `Layer.face`**. The subtree
  renderer now selects main/alpha/mask/province, but steady-state projection always
  draws the main image and `RenderNode` has no face field, so a `dfProvince` or
  `dfMask` layer is only faithful *during* a transition. Making it faithful needs
  province/mask materialized in L2/L1.
* [ ] **R10 (P1, new)** there is **no per-layer affine field**. `RenderNode.transform`
  is never populated; `AffineLayer` bakes its rotation/zoom into the MainImage via
  `onPaint → affineCopy`. Any layer transform the script sets without repainting is
  lost.

### L2 pixel-op math (`audit-layer-ops`)

* [ ] **LOP-1 (P0)** `do_gray_scale` computes `19*R + 183*G + 54*B`
  (`crates/engine/src/layer_ops.rs:1356`); the reference `TVPDoGrayScale_c` is
  `54*R + 183*G + 19*B` (`reference/cpp/core/visual/tvpgl.cpp:10350`). Pure red
  gives 18 instead of 53 — every `doGrayScale` is wrong.
* [ ] **LOP-2 (P1)** `do_box_blur` blurs the wrong channels: it holds alpha when the
  face is not `dfAlpha` and holds RGB when it is. The reference blurs **all four**
  channels and, for `dfAlpha`, additionally unpremultiplies RGB by the blurred
  alpha (`LayerIntf.cpp:5691`, `tvpgl.cpp:10005,10108`).
* [ ] **LOP-3 (P1, D11)** the operation-mode table silently source-overs about 20
  modes through a `_ =>` arm (`layer_ops.rs:1808`): `omAddAlpha` (12), the `omPs*`
  family (14-28) and `omDodge` (8). The reference maps each to its own `TVP*Blend`
  (`LayerIntf.cpp:4118`, `RenderManager.cpp:2396`). Replace the `_ =>` with an
  exhaustive match so a new mode is a compile error.
* [ ] **LOP-4 (P1)** `Appearance.addBrush`/`addPen` require 3/4 arguments; the
  reference defaults `ox = 0, oy = 0` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:162,171`)
  and ncbind passes a missing argument as `void` (`ncbind.hpp:963`), so valid calls
  raise — `layer_ex/appearance.rs:386,425`.
* [ ] **LOP-5 (P1)** `fillRect`'s bitmap path ignores `ClipRect`/`DrawFace`/
  `HoldAlpha` (`layer/mod.rs:1133-1143`); the reference intersects the clip and
  dispatches by face (`LayerIntf.cpp:4272`). Distinct from R3's imageless path.
* [ ] **LOP-6 (P1)** `copy9Patch` derives its geometry from the bottom/right
  `margin` instead of the top/left `scale` and does not abort when `scale` is
  missing (`layer_ops.rs:1542`; `LayerBitmapIntf.cpp:1198-1250`).
* [ ] **LOP-7 (P2)** `convertType`/`light`/`gaussianBlur` act on `ClipRect`; the
  reference acts on the whole image (`LayerIntf.cpp:1984,1006,5749`). The vendored
  `ApplyLightContrast`/`ApplyGaussianBlur` look reconstructed (Chinese design
  commentary, an invented `max(1, radius/2.5)`), so this is probable but unverified.
* [ ] **LOP-8 (P2)** `piledCopy` copies the MainImage instead of `Complete()`-ing the
  source subtree (`LayerIntf.cpp:4529`); hatch `Percent50` paints 25% instead of a
  checkerboard (`raster.rs:209`); `fill_color_on_add_alpha` lacks the `opa == 255`
  `FillARGB` branch (`LayerBitmapIntf.cpp:453`); `affineCopy` holds alpha on
  `dfAlpha` where the reference uses `hda = false`.
* [ ] **LOP-9 (oracle choice)** the vendored Windows `LayerExDraw` uses GDI+
  `HatchBrush` while the blend2d build has its own `createHatchPattern`, and they
  disagree on `DiagonalBrick` (style 38, which `advscreen.tjs:3033` uses). Pick and
  record the oracle — it also decides the `LayerExFont` leading metrics.

### Plugin-family natives (`audit-plugin-natives`)

`native_parity` checks only core `.cpp`, and `plugin_parity` is blind to every
**post-registration global**, so this surface has no parity coverage at all:
`dirlist`'s `getDirList`, `fftgraph`'s `drawFFTGraph`, `fstat`'s
`FILE_ATTRIBUTE_*`, `motionplayer`'s `ShortCutInitial*`, and the whole real
`WIN32Dialog` surface. 100% parity is not coverage.

Several vendored plugin sources are **stubs or reconstructions**, so "match the
reference" is undefined for them: `json/jsonPlugin.cpp` is a self-labelled demo
that returns its input strings, `win32dialog.cpp` is 12 lines (`messageBox`
only), `windowEx.cpp`'s `System` returns `TJS_S_OK` without writing a result, and
`psbfile` has `TODO` bodies. The port implements the *real* plugin in some cases
and the stub in others — each needs an explicit oracle decision.

* [ ] **PN1 (P1)** `motion_player` is missing the `ShortCutInitial*` globals.
* [ ] **PN2 (P1)** `xp3_filter`: the buffer object is an `Array` carrying only
  `xor`; the reference's `CBinaryAccessor` also has `ptr`/`count` and its
  operators, and the content filter runs **per entry**, not per segment.
* [ ] **PN3 (P1, oracle)** decide the `json` oracle (see above) and record it.
* [ ] **PN4 (P2)** `fstat`: raise/validation paths, `moveFile` deleting on
  failure, and the `getDisplayName`/`copyFileNoNormalize` archive handling.
* [ ] **PN5 (P2)** `dirlist`: `getDirList` raises instead of returning `[]` for a
  missing directory, and there is no `S_IFREG|S_IFDIR` filter.
* [ ] **PN6 (P2)** `scripts_ex`: `safeEvalStorage` drops `context` and the mode;
  `storage_name` misses `>`; `getObjectCount` returns a hard zero for arrays.
* [ ] **PN7 (P2)** `windowEx`'s `System` oracle; the `layer_ex` `Appearance`
  argument defaults (LOP-4); a dead filter in `save_struct`; the `psd://`/`psb://`
  seams.
* [ ] **PN8 (P3)** `windowEx` writable `ncht*`; the `steam` synthetic
  `interface`; teach `scripts/plugin_parity.py` to see post-registration globals.
* [x] **PN9 (do not "fix")** reference-stub members that must stay as they are:
  `Point::contains`, `setMaxTextureSize`, `TextRenderBase.newline`/`setKeyWait`,
  and the `fstat` no-op returns.
* [ ] **PN10 (P0, deferred)** the real `WIN32Dialog` surface — the platform dialog
  layer (§4).

### Input / audio / video (`audit-media-input`)

* [ ] **M1 (P0)** the key code handed to scripts is the winit `KeyCode`
  **discriminant**, not a Win32 `VK_*`. `crates/platform/platform/src/linux.rs:291-303`
  emits `code as u32` and `crates/compat/src/event.rs:176-184` passes it through, so
  `system/window.tjs:119-193`'s `key == VK_RETURN`/`VK_F12`/`VK_UP` comparisons never
  match and every KAG keyboard shortcut is dead. It also breaks `shift_flags`
  (`natives/input.rs:274-293`), which probes VK `0x10`/`0x11`/`0x12` against that
  table, so `ssShift`/`ssCtrl`/`ssAlt` are always 0 — Alt+Enter, Ctrl-skip and
  `Key.getPressed` too. The L1 doc already notes that an explicit key enum should
  replace the cast.
* [ ] **M2 (P0)** video is non-functional: `app/game.rs` never calls
  `set_video_source`/`set_movie_source`, so `VideoOverlay.open` and
  `Layer.openMovie` raise; `video_poll`/`movie_poll` are wired but moot.
* [ ] **M3 (P1)** `onCloseQuery` is registered as `window_noop_handler`
  (`window.rs:1958`) and `main.rs:529` consumes `CloseRequested` and exits, so L3
  never fires it; `Window.close()` removes the window outright. The reference runs
  `OnCloseQueryCalled` with a veto (`TVPWindow.cpp:790-880`).
* [ ] **M4 (P1)** `MIDISoundBuffer.open`/`play` actually load and play
  (`cdda_midi.rs:229,491`); the reference guards them with `TVP_ENABLE_MIDI`, which
  is defined nowhere in the tree, so they must be inert (CDDA is already correct).
* [ ] **M5 (P1)** `getVisBuffer` is a stub returning 0 (`sound.rs:1597`); the
  reference fills `dest` and returns a count, and `useVisBuffer = true` allocates a
  live buffer (`reference/cpp/core/sound/win32/WaveImpl.cpp:3401-3556`).
* [ ] **M6 (P1)** `onMouseWheel`'s delta is `±1`; the reference posts `±120`
  (`event.rs:167-175` vs `TVPWindow.cpp:109-115`).
* [ ] **M7 (P1)** `VideoOverlay` frames are never composited — `layer1`/`mode`/
  `setMixingLayer` are stored, but nothing reads `current_frame()` outside the
  layer-ex movie path.
* [ ] **M8 (P1)** `Layer.openMovie`/`startMovie` raise where the reference logs and
  returns (`layerExMovie.cpp:160-227`), changing what a `try/catch` observes.
* [ ] **M9 (P2)** window-level `onMouseEnter`/`onMouseLeave`/`onDoubleClick` are
  synthesized with no reference poster; `onMouseDown` does not first run
  `PrimaryMouseMove` and `onMouseUp` does not re-check the layer under the cursor
  (`LayerManager.cpp:374-419`); the `Mouse`/`Key` native classes and
  `Mouse.getWheelRot*` are **port-only** (the reference registers neither — and
  `natives/input.rs` is absent from `native_parity`'s map); `PlayStatus` lacks
  `"ready"`; coordinates are physical rather than the reference's primary-layer
  node space (latent while `setZoom` is inert).

### File system / XP3 (`audit-fs-xp3`)

Every `.xp3` under `/mnt/DATA/Games` mounts and every name normalizes. The one
anomaly (`/mnt/DATA/Games/Others/doinaka_brother`) is a title-specific
encrypted-name DRM variant, **not** a parse bug — do not "fix" it in the storage
layer.

* [x] **F5 (P1)** the filter now receives the **normalized** name
  (`crates/platform/fs/src/xp3.rs:361`) — landed with the fs/XP3 batch.
* [ ] **F1 (P1)** `Storage::mount_archives` aborts on the first bad `*.xp3`
  discovered by `app/src/game.rs:152-159` (`engine/storage.rs:182-191`); the
  reference opens archives lazily and defensively (`StorageIntf.cpp:704-724`,
  `StorageImpl.cpp:562-587`), so one stray or corrupt archive stops the game.
* [ ] **F2 (P1)** `discover_archives` sorts ASCII-wise (`engine/storage.rs:858-898`),
  so `patch@r996` beats `patch@r1071` and `patch_append5` beats `patch_append10` —
  the wrong patch order for a numbered-patch game.
* [ ] **F3/F4 (P1)** `find` searches disk → every mounted archive → auto paths
  (`engine/storage.rs:341-404`); the reference searches the current path (project
  archive) first, then auto paths by base name, and opens auto-path archives **on
  demand**. Loose-vs-archive priority and `--archive`-only launches diverge.
* [ ] **F6 (P1)** `Storages.getFullPath`/`getPlacedPath` return relative names; the
  reference returns the full `file://domain/path` (`StorageImpl.cpp:1284-1290`,
  `StorageIntf.cpp:1080-1106`).
* [ ] **F7 (P1)** `exists` reports directories as existing
  (`engine/storage.rs:638-657`); the vendored oracle is regular-file-only
  (`StorageImpl.cpp:530-541`). But the code, its test and
  `docs/migration/reviews/fs_engine.md` all disagree with the vendored reference,
  so settle it with shipped-engine evidence before changing anything.
* [ ] **F8-F14 (P2/P3)** `..`/absolute traversal refused; `getLocalName` misses
  return the input; `isExistentStorageNoSearchNoNormalize` is case-sensitive; the
  wrong `addAutoPath` message text; the filter buffer lacks `ptr`; host write
  semantics; parser strictness. F15/F16 are dead or duplicated surface.

### Core native behaviour (`audit-core-natives`)

One bug class dominates: **the port raises where the reference is inert and
returns a benign value.** D32 says a member behaves like the reference or raises
*because the reference raises*; inventing an error breaks a script that the
reference lets run. For the `#if 0` members below the reconstruction cannot show
the real Windows side effect, but the reference still returns a value — so match
that and log a warning (the null-plugin precedent), never raise.

* [ ] **C1 (P0)** `System.system(t)` raises; the reference returns `0` and runs
  nothing (`SystemImpl.cpp:674-691`) — `system.rs:1306`.
* [ ] **C2 (P0)** `System.shellExecute` raises; the reference always returns `true`
  (`SystemImpl.cpp:77-88`) — `system.rs:1261`.
* [ ] **C3 (P0)** `Storages.searchCD` raises; the reference returns `""`
  (`StorageImpl.cpp:1232-1246`) — `storages.rs:543`.
* [ ] **C4 (P0)** `Storages.selectFile` raises; the reference returns `false` with no
  picker (`FileSelector.cpp:100,378-399`) — `storages.rs:577`. A real picker is the
  platform dialog layer's job (§4).
* [ ] **C5 (P0)** `Font.doUserSelect` raises; the reference returns `0`
  (`LayerIntf.cpp:11931-11956`) — `font.rs:616`.
* [ ] **C6 (P0)** `Font.rasterizer = n` raises for `n != 0`; the reference ignores an
  out-of-range value (`LayerBitmapImpl.cpp:94-101`) — `font.rs:825`.
* [ ] **C7 (P0)** `Scripts.exec`/`eval` drop the `context` (the `this`) and
  `lineofs` arguments (`ScriptMgnIntf.cpp:1385-1438`) — `scripts.rs:378,409`.
* [ ] **C8 (P0)** `Scripts.execStorage`/`evalStorage` drop `context` and the non-`o`
  mode (encoding), and use the full storage name as the block name where the
  reference uses `TVPExtractStorageName` — `scripts.rs:293,334,196`.
* [ ] **C9 (P1)** `System.exitOnNoWindowStartup` defaults to `false`; the reference
  defaults `true` (`SysInitImpl.cpp:383`).
* [ ] **C10 (P1)** `System.assignMessage` always returns `true` and the override map
  is never read.
* [ ] **C11 (P1)** `Font.getList` ignores `flags`.
* [ ] **C12 (P2)** `System.inform` always returns `0` (the reference returns a button
  index); `System.versionInformation` text is port-specific; `System.touchDevice`
  invents touch bits (the reference is always `0x300`); `Bitmap.save` ignores `meta`
  and coerces `""` to `bmp`; `Bitmap.loadHeader` raises where the reference returns
  void; `Plugins.getList` rejects arguments the reference accepts;
  `Debug.logLocation` does not clear unreachable locations or honour `-forcelog`.
* [x] **C13** `Storages.addAutoPath` (P2-2b) is **stale** — L3 already enforces the
  trailing delimiter; only the raised text differs from
  `TVPMissingPathDelimiterAtLast` (`StorageIntf.cpp:924-927`).
* [ ] **C14 (coverage gap)** `native_parity` machine-checks only core `.cpp`; the
  runtime surface also carries `fstat`/`dirlist`/`xp3_filter`/`scriptsEx`/`windowEx`
  members whose behaviour no audit has checked. Their references live under
  `reference/cpp/plugins/**` and need a separate pass.

### Text / fonts (render audit R4/R6 + the text-render diagnosis)

* [x] **TEXT-1** the `drawText` shadow now convolves each character's coverage with
  the reference cone kernel (`tvpgl.cpp:10770-10813`, including
  `fast_int_hypot` at `:10738-10763`), sums with saturation
  (`TVPChBlurAddMulCopy65_c`), composites the mask once with `shadowcolor` at the
  draw opacity, and scales a zero-width blur by `blurlevel/256`
  (`CharacterData.cpp:118-160`) — landed `3313271`, made exact in `3ed61ca`.
* [x] **TEXT-2** `Font.height` must normalize a negative value to positive
  (`LayerIntf.cpp:5962-5969`) and `Font.angle` must normalize modulo 3600
  (`:5975-5988`). Landed `3313271`.
* [x] **TEXT-3** `Font.faceIsFileName` — implemented: stored per font id in
  `FontMetricsState.face_is_file_name`, cleaned up on destroy, get/set round-trip
  (`LayerIntf.cpp:11677-11693`).
* [x] **TEXT-4** the silent blank text — `rasterize_text_coverage` now raises
  `TextError::Glyph(GlyphError::MissingGlyph)` when a run's non-whitespace
  characters all rasterize to empty coverage (whitespace/controls still yield
  nothing legitimately), covering the `resolve_face → default_font()` case
  (`FreeTypeFontRasterizer.cpp:185-194`).

### KAG parser (`kag-compat-audit` §B)

* [~] **B.2-1** register `multiLineTagEnabled` (and the Ex `paramMacros`).
* [~] **B.2-2** consume `[iscript]`/`@iscript` and fire `onScript`
  (`KAGParser.cpp:1097-1188`, `TVPKAGInlineScriptNotEnd` `:1154`,
  `TVPLabelOrScriptInMacro` `:1130`).
* [~] **B.2-3** owner callbacks and vetoes — `onScenarioLoad`, `onScenarioLoaded`,
  `onJump`, `onCall`, `onReturn`, `onAfterReturn` (`:919-937,955-966,1676-1762,1341-1350`).
* [~] **B.3-1** 0-based `curLine`, whole-line `curPos`, consistent `curLineStr`
  (`:987-991,1015-1053,1080-1082`).
* [~] **B.3-2** `macros` must be the live table, not a cached copy.
* [~] **B.3-3** `store`/`restore` must carry `lineBuffer`/`lineBufferUsing`
  (`:506-634`).
* [~] **B.3-4** stop silently dropping `%`-prefixed lines (`:1490-1560`).
* [~] **B.4-1** duplicate/empty label aliasing (`:193-238,1032`).
* [~] **B.4-2** `assign` must not copy `ProcessSpecialTags`/`Interrupted`
  (`:395-466`); raise the bare `TVPKAGSpecifyKAGParser` text (`:59-60`).
* [ ] **B.4-3** macro-arg default when the argument is present but void
  (`:2110-2113`).
* [ ] **B.4-4** `*` macro-entity injection gating (`:1877-1891`).
* [ ] **B.4-5** `curLineStr` must return the expansion buffer while
  `LineBufferUsing` (`:1080-1082`).
* [ ] **B.4-6** bracket-tag `ignoreCR` is decided after the handler
  (`state.rs:678-726`).
* [ ] **B.4-7** Ex `taglist` shape — unverifiable from the vendored reference.

### Plugins / Win32 (`kag-compat-audit` §A)

* [x] **A.3-2** `getDisplayMonitors`/`getMonitorInfo` nested `work`/`monitor`
  shape — landed `c2b785b`/`16cd7cf`. Open sub-item: the real `windowEx.dll`
  monitor dict also carries a `name` string (MONITORINFOEX `szDevice`) that the
  host monitor tuple cannot supply, and the shipped shim never reads it.
* [x] **A.3-3** alias `KAGParser.dll` → `KAGParserEx.dll` — landed `c2b785b`.
* [ ] **A.3-1** the real `WIN32Dialog` surface — deferred to the platform dialog
  layer (§4).
* [x] **A.3-4** `windowEx`/`win32dialog` recognition — no divergence found.

### Flow / scene / save (`kag-flow-audit`)

* [x] **P0-1** `beginTransition` + `withchildren` (same as R2) — landed `a63b9fb`.
* [x] **P0-1b** the subtree renderer applies each child's placement
  (`ImageLeft/Top/Width/Height`, `rect`, `ClipRect`), `face` (main/alpha/mask/
  province), `holdAlpha` and blend (`a63b9fb`). A per-layer **affine** transform is
  still not carried: the port bakes `AffineLayer` rotation/zoom into the MainImage
  via `onPaint → affineCopy`, and `RenderNode.transform` is never populated, so a
  layer mid-affine still renders from its baked bitmap (see R10).
* [~] **P1-1** `Layer.saveLayerImage` must write the reference BMP layout. The game
  appends the save struct at `width*height*4 + 54` (`utility.tjs:2205`,
  `advscreen.tjs:7286,7437,7440`), which the reference `TVPSaveTextureAsBMP`
  produces (40-byte `TVP_WIN_BITMAPINFOHEADER`, `bfOffBits = 54`,
  `GraphicsLoaderIntf.cpp:737-800`). The port's `image`-crate writer emits a
  108-byte V4 header (`w*h*4 + 122`), so `saveStruct(path, "zoN")` overwrites the
  thumbnail's last 68 bytes and a reference-compatible reader rejects the file
  (`GraphicsLoaderIntf.cpp:399-420`).
* [x] **P2-3** `exchange_layers` re-parents and reorders the pair, for both the
  `Exchange` (`keepchildren=false`) and `Swap` (`true`) forms (`LayerIntf.cpp:754-979,
  7969-7991`) — landed `a63b9fb`.
* [x] **A.3-2b** `System.getMonitorInfo(flag, window|x,y|x,y,w,h)` resolved from the
  shipped `windowEx.dll` jump table — landed `16cd7cf`.
* [x] **P2-2** `Storages.addAutoPath` dedupe-without-reorder — **verified not a
  divergence**: the reference does exactly the same (`StorageIntf.cpp:921-937`).
* [ ] **P2-2b** the reference does raise `TVPMissingPathDelimiterAtLast` for a path
  without a trailing `/`, `\` or `>` (`StorageIntf.cpp:924-927`); the port accepts
  it (`engine/storage.rs:315-320`).
* [x] Verified consistent: the scene manager, timers, `AsyncTrigger`,
  `addContinuousHandler`, window event dispatch, `VideoOverlay`, sound, the
  `saveStruct`/`evalStorage` offset path, and the ADV/album/screenshot rect ops.

## 6. Input / IME — implemented, with documented imprecisions

Landed in `b1372cf`. These are knowingly imprecise; each needs a decision or a
reference source before it can be made exact:

* **Click / double-click timing.** The vendored reference declares
  `tTVPOnDoubleClickInputEvent` but never posts it (no `WM_LBUTTONDBLCLK`, no
  `GetDoubleClickTime`); the cocos2d path synthesizes `onClick` only
  (`TVPWindow.cpp:295-315`). The port reuses its Windows-derived sequence
  (`MOUSE_CLICK_SEQUENCE_FRAMES=30`, `MOUSE_CLICK_MAX_MOVE=4`) and fires
  `onDoubleClick` on the **second release** (Windows fires it on the second
  press). Needs the real thresholds from a shipped binary or the game's data.
* **Touch scaling / rotate.** `onTouchScaling`/`onTouchRotate` are registered but
  never synthesized: no reference platform recognizer posts
  `tTVPOnTouchScaling/RotateInputEvent`, and the `flag` values are never set
  anywhere in `reference/cpp/**`. Guessing them would be an invented
  approximation.
* **Layer-native action owner.** The reference layer `on*` natives also call
  `ActionOwner.action(event)` (`LayerIntf.cpp:476,9970-…`); the port forwards only
  the window path, because doing both would fire `action` twice per event. Decide
  how KAG expects the window and layer events to relate.
* **Window `onMouseEnter`.** Emitted on the first `PointerMoved` after a leave —
  there is no `CursorEntered` platform variant; the reference detects it in the
  stateful layer manager (`LayerManager.cpp:424-468`), not in a raw event.
* **`onMouseDown` also runs `PrimaryMouseMove`** in the reference (so a down can
  emit `onMouseMove`); the port relies on the platform's separate move events.

## 7. Refactor: reshape the internal code the plugin-extras audit left behind

The audit (`16583bc`) deleted 30 port-only plugin members. Two pieces of internal
code were kept for behaviour but should be reshaped to the house pattern
(a shared behaviour **struct** composed by the thin classes, D16 — not a
per-class flag list/registration copy):

* `window_ex` — `window_inert` (the reference `TJS_S_OK` stubs used by `minimize`,
  `maximize`, `showRestore`, `resetWindowIcon`, `setWindowIcon`, `bringTo`,
  `sendToBack`) and the `WindowFlags` set (`disable_resize`, `enable_nc_mouse_event`,
  `message_hook_all`, `sys_menu`, `overlay`) plus `WindowExState`/`with_window`.
  Collapse the inert stubs and the per-object flag map into one shared struct with
  named operations.
* `objthis_has_member` is the dynamic-probe helper for both `Window.registerExEvent`
  and `Pad.registerExEvent`; the probe results are no longer stored. Fold the probe
  into the same shared struct so the two callers do not each re-implement it.
* `ModuleSettings::max_texture_size` was already deleted (write-only; the reference
  `setMaxTextureSize` stores nothing). Confirm nothing else in `motion_player`
  retains write-only state.

## 8. L3 blockers — must behave like the reference

Verify current state first; several were fixed in the last wave.

| member | file | what the reference does | what it needs |
|---|---|---|---|
| `Window.registerMessageReceiver` | `natives/visual/window.rs` | registers a Win32 message receiver and delivers messages | a receiver registry + dispatch from the event loop |
| Platform-menu operations (`MenuItem`, `MenuItemEx`) | `natives/menu_item.rs`, `plugins/window_ex/menu_item.rs` | creates and operates a real `HMENU`; `popupEx` shows it | a synthetic menu-handle registry behind `Window` |
| `DrawDevice.interface` / `DrawDeviceForSteam.interface` | `natives/visual/draw_device.rs`, `plugins/steam/mod.rs` | returns the underlying `iTVPDrawDevice *` | an L1/L2 object to hand back, or a faithful stand-in |
| `MIDISoundBuffer.midiOut`, CD track selection | `natives/cdda_midi.rs` | opens a Win32 MIDI/CD device and reports state | a synthetic device reporting plausible state |
| Class-name constructor member | the ABI/factories | every reference class declares `Constructor()` | classes registered without a class-named method cannot be constructed via `super.X()` (the ABI installs the ctor dispatch only for a class-named method, `tjs2_abi.cpp:2268-2286`); check which classes still lack it |
| `layer_ex` movie playback | `plugins/layer_ex/movie.rs` | advances per frame | `movie_poll` is unit-tested but **no host wires it**, so a movie does not advance in game |
| `windowEx` dynamic events | `plugins/window_ex/window.rs` | fires `onMinimize`/`onResizing`/… when the window state changes | the port matches the dynamic lookup but never raises them (no Win32/NCHITTEST source on Linux) |
| Hatch fill, linear gradient, `LayerExFont` leading metrics | `engine/src/layer_ops.rs`, `plugins/layer_ex/{appearance,font}.rs` | exact pixels / real metrics | approximations today — forbidden by D32; give L2 the missing primitive, never patch L3 |

## 9. Platform stubs — Android only (permitted)

* `crates/platform/platform/src/android.rs` — window + event loop return `NotImplemented`; the AAudio output backend is real. Needs `ANativeWindow` + `GameActivity`, a size/scale source, a redraw request reaching the Vulkan surface, and input→`PlatformEvent` translation (`docs/migration/10-android-backends.md`).
* `crates/platform/media/src/video/mediacodec.rs` — written, **never compiled or run**; the host build uses a typed `stub`.

## 10. Deferred design work

* **L2 owns the frame** — `Engine::frame`/`Engine::run`/`GameFrame` designed, not built (`docs/migration/02 §3`, `07 Phase 9`). The current L3 clock is `platform::Clock`, a wall clock: a headless run of N frames reaches real time only by accident, which is why timer tests need care.
* **TJS VM Rust port** — deferred (`08 §10.7`).
* **L2 API snapshot + CI build step** — `cargo public-api` decided, unwired; CI runs only `cargo fmt --check` + the stateless guard, so the surface lock is a local gate.
* **Windowed audio on a real device** — owed.

## 11. Quality backlog

* **Harness hardening** — encode the §9 traps so they cannot recur: an `eval`-that-reads helper, a "not the primary layer" helper, and a fixture documenting the TJS `typeof` spellings.
* **Remaining bean-style accessors** — `InputState::set_pointer_visible`, `SoundChannel::looping`/`set_looping`, `Mixer::set_app_focused`/`set_app_minimized`, `Storage::set_xp3_extraction_filter`, `TimerState::set_now`, `VideoOverlay::set_bounds`/`set_looping`.
* **`VideoOverlay::new(layer, window, alpha, visible)`** — four positional args incl. a `bool`; wants a builder or `new(layer)` + field writes.
* **`#[non_exhaustive]`** — public structs/DTOs yes; enums L3 matches exhaustively no (a `_` arm turns a compile-time guarantee into a silent fallback).
* **`unsafe_code` lint** — `forbid` where there is no `unsafe`, `deny(unsafe_code)` + a file-level `allow` where the FFI lives.
* **`get_`-prefixed public API left:** `Tjs2Engine::get_member`, `get_class_names`; `Scene::get_province_pixel`.
* **Verify no decision-number tags returned** to plugin sources: `grep -rn "//.*\bD[0-9]\+\b" crates/compat/src`.

## 12. Traps that cost real time — read before triaging anything

1. **`typeof` uses TJS spellings:** `"Object"`, `"Integer"`, `"Real"`, `"String"`, `"Octet"`, `"void"`. Only an **absent member** yields `"undefined"` (`tjsInterCodeExec.cpp:1899-1904`); present-but-unset is `"void"` (`:2601-2628`).
2. **`void` is not `undefined`.** A bare `undefined` is a member lookup and raises; test absence with `typeof x === "undefined"` (or `typeof global.x`, since a bare `typeof <absent global>` also raises in this VM).
3. **`property` is a TJS keyword.** `var property = …` is a syntax error.
4. **Inside a subclass body, a bare native class name resolves to `this.<Name>`** — the copied non-static constructor function, whose `new` is `TJS_E_NOTIMPL`. Write `new global.Layer(…)`.
5. **`Env::eval` compiles in expression mode**: `eval("a = 1; a")` never runs the read. Write-then-read must be **separate executions**.
6. **The first layer created on a window is force-visible** (`LayerManager.cpp:161-172`).
7. **The compat binding is process-global and single** — drop the first `Env` before creating a second in one test.
8. **A dictionary swallows a missing read as `void`**; a custom object reports absence (`tjsDictionary.cpp:739-743`). Use `typeof o[k]`, not `get_member(…).is_err()`.
9. **Vendored C++ is not automatically right.** Where `reference/cpp/**` is a reverse-engineered reconstruction it can contradict the documented format and the shipped data (`json`, `motion_player` PSB layout). Say so explicitly, with the evidence.
10. **TJS2 does not bind `function` declarations inside an anonymous function body** (an IIFE wrapper breaks them). A **named global `function` declaration also binds `this` to the global object** — for a namespace helper, use an anonymous function expression assigned to the namespace member and recurse through `this`.
11. **`Layer.id`/`nativeId`, `Bitmap.id`, `Window.id`, `Font.id` do not exist** by design; identify through real members or the L2 `Scene`.

## 13. Verified behaviour worth not re-opening

XP3 parsing rejects malformed archives with typed errors (no panics); `media` decodes image/audio/video with no silent fallbacks; `wuvorbis` exposes no TJS surface by design; the null-plugin object exists only for DLLs never ported and announces itself; the compat binding is installed exactly once per process; `plugin_parity`/`native_parity` are at 100% with an empty allowlist.

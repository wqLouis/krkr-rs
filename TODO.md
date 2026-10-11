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
2. **`Layer.beginTransition` — game blocker** — in flight. `Sprite.tjs:290` calls
   `super.beginTransition(name, withcildren, transsrc, options)`; the reference passes
   `withchildren` (default **true**, `LayerIntf.cpp:9819-9821`) to `StartTransition`,
   which requires the endpoint main images **only when `!withchildren`**
   (`:7860-7868`), otherwise transitions the whole tree through the cache
   (`:7869-7876`).
3. **`Layer` has no default image** — see R1 below; dispatched next.

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
* [~] **R2 (P0)** `beginTransition`'s `withchildren` (`:9819-9821,7860-7876`).
* [ ] **R3 (P1)** `fillRect` must fill only `rect ∩ ClipRect` inside `MainImage`
  (`:4272-4320`); the port-only `layer.fill_color` must go.
* [~] **R4 (P1)** prerendered `.tft` fonts (`:11790-11796`, `:11716-11746`).
* [ ] **R5 (P2)** `setImageSize` must shrink the rect and clamp `ImageLeft/Top`
  (`:2623-2640`); `setImagePos` must reject out-of-range positions (`:2506-2523`).
* [~] **R6 (P2)** `Font.getTextWidth`/`getTextHeight` must apply `angle`
  (`:11716-11746`).
* [x] **R7 (P2)** `System.stayOnTop` — **verified not a reference member** (only
  `Window.stayOnTop`, `WindowIntf.cpp:1608`); the script writes a dynamic member.
  Nothing to add.
* [~] **R8 (P2)** `Layer.hasImage` must report true on a fresh layer (`:2497`);
  falls out of R1.

### Text / fonts (render audit R4/R6 + the text-render diagnosis)

* [~] **TEXT-1** the `drawText` shadow must be spread by `shadowwidth` (the
  reference blurs and widens the shadow bitmap by `abs(blurwidth)` on every side,
  `CharacterData.cpp:118-160`; a shadow exists only when `shadowlevel != 0`,
  `LayerBitmapImpl.cpp:1168-1178`). Semantics landed `3313271` — the game draws
  white text with a zero-offset near-black shadow, so without the spread the white
  had no halo and disappeared on a light background. **The rasterization still
  diverges**: the reference `TVPChBlurCopy65_c` (`tvpgl.cpp:10770-10813`) convolves
  each character's coverage with a **cone** kernel
  `weight = ((bw - fast_int_hypot(dx,dy) + 1) * lvsum * blurlevel) >> 8` and
  **sums with saturation** (`TVPChBlurAddMulCopy65_c`), whereas the port stamps the
  sharp glyph at an equal weight per offset and alpha-composites it.
* [x] **TEXT-2** `Font.height` must normalize a negative value to positive
  (`LayerIntf.cpp:5962-5969`) and `Font.angle` must normalize modulo 3600
  (`:5975-5988`). Landed `3313271`.
* [ ] **TEXT-3 (D32)** `Font.faceIsFileName` is still a no-op stub returning 0
  (`font.rs:387-390`). It needs per-font state in the engine's font record.
  `resolve_face` already tries the storage for a file-name face, so the reader is
  the missing half.
* [ ] **TEXT-4 (D11)** in `crates/engine/src/text.rs`, when `resolve_face` falls
  back to `default_font()` on a host whose faces carry no glyph for the text,
  `compose_text_into` skips the empty masks and the run renders blank with no
  error. Choose a fallback that can cover the text or raise a typed error.

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

* [~] **A.3-2** `getDisplayMonitors`/`getMonitorInfo` nested `work`/`monitor`
  shape; the shipped `k2compat_deskinfo`/`k2compat_scrinfo` are the oracle.
* [~] **A.3-3** alias `KAGParser.dll` → `KAGParserEx.dll`.
* [ ] **A.3-1** the real `WIN32Dialog` surface — deferred to the platform dialog
  layer (§4).
* [x] **A.3-4** `windowEx`/`win32dialog` recognition — no divergence found.

### Flow / scene / save (`kag-flow-audit`)

* [~] **P0-1** `beginTransition` + `withchildren` (same as R2) — landed by
  `fix-transition`.
* [~] **P0-1b** the subtree renderer must apply each child's transform, mask and
  province. `layer_ops::collect_subtree_draws` copies only
  `rect`/`image_left`/`image_top`/`clip`/`blend`/`opacity`/`bitmap`/`fill_color`, so
  a mid-activation `Sprite`/`AffineLayer` child renders unscaled and unrotated
  while the reference's `Complete` draws it through its own transform. Every
  reachable ADV transition passes `withchildren = true` with a container source
  (`advscreen.tjs:216-217,3353,3719,3735,3737,3739`, `advobject.tjs:105`).
* [~] **P1-1** `Layer.saveLayerImage` must write the reference BMP layout. The game
  appends the save struct at `width*height*4 + 54` (`utility.tjs:2205`,
  `advscreen.tjs:7286,7437,7440`), which the reference `TVPSaveTextureAsBMP`
  produces (40-byte `TVP_WIN_BITMAPINFOHEADER`, `bfOffBits = 54`,
  `GraphicsLoaderIntf.cpp:737-800`). The port's `image`-crate writer emits a
  108-byte V4 header (`w*h*4 + 122`), so `saveStruct(path, "zoN")` overwrites the
  thumbnail's last 68 bytes and a reference-compatible reader rejects the file
  (`GraphicsLoaderIntf.cpp:399-420`).
* [~] **P2-3** `exchange_layers` must re-parent and reorder the two layers
  (`LayerIntf.cpp:754-979,7969-7991`); the port swaps only `x/y/visible/z_order`
  (`transitions.rs:790-830`), and its in-file comment claiming the re-parenting is
  unmodelled is now stale.
* [~] **A.3-2b** `System.getMonitorInfo(flag, window)` — the shipped shim passes
  `(true, global.Window.mainWindow)` and reads the returned dict, not an index;
  the port treats argument 0 as a monitor index and the app installs only index 0.
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

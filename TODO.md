# TODO — open work

**Branch:** `main` · **repo:** `/mnt/DATA/Document/code/krkr-rs`
**Real game:** `/mnt/DATA/Games/Others/test` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

This file lists **only what is still open**, verified against the tree by four
read-only audits (rendering, natives/KAG/plugins, media/input/fs, infra) whose
findings are folded in below with the landing commits they cite. Behaviour that
was checked and found *not* to be a divergence lives in §13 so it is not
re-opened. History is read with `git log`.

---

## 0. Conventions

* **Subagents must not run `cargo`** (`build`/`check`/`clippy`/`test`/`run`) — parallel builds once exhausted a 16 GB tmpfs and broke the tree. They may use `rustfmt`/`cargo fmt`, run prebuilt artifacts (`target/debug/krkr`, `target/debug/deps/*`), and run the Python scripts. The main agent runs the gate.
* **Only the main agent runs `uv run --project scripts krkr-gate surface-lock --update`.**
* **Comments** (D31): describe the code as it is — no history, no "used to", no rationale, no decision numbers. Keep `// SAFETY:`, invariants, and reference citations **in tests**.
* **Tests** carry a `reference/cpp/...:line` citation above every behaviour assertion. An assertion without one is not evidence.
* **Printed/logged text must not use internal jargon** (`L1`–`L4`, decision numbers). Tests are exempt.
* **D32:** only the Android backend may be stubbed; L3 may not stub and may not approximate. `#[allow(dead_code)]` is banned (one exception: `crates/compat/tests/common/mod.rs`).
* **Evidence** (D36): a compatibility claim names what was asserted and where — host test, device run, or both — and a knowingly-left divergence is recorded as an approximation, not an "unobservable". An engine release needs at least one real KiriKiri title run on a real device with its log captured (`<filesDir>/krkr.log`; §11 and `docs/android.md` §7).
* **Traps that cost real time live in `AGENTS.md` §9** and are deliberately not duplicated here.

## 1. The gate (one command)

```bash
uv run --project scripts krkr-gate gate
```

`krkr-gate gate` runs the ordered **static** sequence: `cargo fmt --all --
--check`; the `stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `native-construction`, `storage-media`, `callback-objthis`
checks; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo clippy
-p platform --features platform-android --all-targets -- -D warnings`;
`android-target`; `surface-lock --update`; `surface-lock`; `native-parity`;
`plugin-parity --no-doc`; `host-facts`.

The test suites are separate: `uv run --project scripts krkr-gate test` runs
`cargo test --workspace --no-fail-fast`, then the `platform` suites for
`platform-linux` and `platform-android`. A change is done only when both the
static gate and the test suites are green.

The compiler-free checks also run individually with no build (`android-target`
shells out to `cargo tree` only): `uv run --project scripts krkr-gate <check>`
for `<check>` in `stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `native-construction`, `storage-media`, `callback-objthis`,
`host-facts`, `android-target`, `native-parity`, `plugin-parity`, or
`surface-lock --no-regen`; `gate --no-cargo` runs the compiler-free subset in
the same order.

The `platform-android` steps build the **feature on the host**, not the Android
target: there is no NDK on the development host and none may be installed.
Android-only code is documented as unverified rather than described as working.

Real-game oracle (expect no `WARN`/`ERROR`, `texture >= 1`):

```bash
target/debug/krkr --headless --frames 10000 --base-dir /mnt/DATA/Games/Others/test
```

A directory-only launch mounts every `*.xp3` under the base directory
recursively; an explicit `--archive` list suppresses that (AGENTS §9 trap 12).

## 2. Focus model

The per-root domain change landed (`96207a6`): a focus domain per parentless
layer, the window bound to the first, a cross-domain re-parent raising
`TVPCannotMoveToUnderOtherPrimaryLayer`. Two residuals:

* **Input is still flattened across domains.** Hit-testing walks every window
  layer (`crates/compat/src/event.rs` → `Scene::layer_at`), where the reference
  binds input to manager index 0 (`LayerTreeOwnerImpl.h:21`), so a click can
  reach a layer in a second root that the reference would never deliver it to.
  The engine-side lookup exists (`Scene::window_input_root`); the dispatch does
  not use it. Changing this will need the multi-root input fixtures rebased the
  way the focus fixtures were.
* **`is_primary`/`primary_layer` is per window, not per manager.** Every
  parentless root is the primary of its own manager (`LayerIntf.cpp:1217-1222`),
  so hiding or moving a *second* root should raise
  `TVPCannotSetPrimaryInvisible`/`TVPCannotMovePrimaryOrSiblingless`; the port
  only raises for the window's first layer.

## 3. Real-game reports

1. **SIGSEGV while saving — open.** `docs/audits/save-crash.md` ranked four
   causes: C1 (text stream), C3 (instance-member leak) and C4 (`getVisBuffer`
   raw pointer) landed in `9a8f784`/`b6188f7`. **C2 remains**: the serializer
   recurses without a depth bound (`crates/tjs/cpp/tjs2/tjsArray.cpp:1161`,
   `tjsDictionary.cpp:642`; cycle guard only, identical to the reference), and
   the decisive backtrace was never taken. Close it by reproducing once and
   reading the backtrace, not by guessing.
2. **The game cannot change resolution — reworked into automatic Fit.** The
   first attempt (`2b021c6` scaling, `dbc1e9b` resize propagation) scaled the
   composited tree and is being removed (`docs/migration/11` §4). The
   replacement presents every game uniformly scaled and letterboxed to the
   device size (D39), so the game no longer needs to change the framebuffer
   resolution; the script's own declared size (`System.screenWidth`,
   `Window.setSize`) is unchanged and still what the script sees. Still owed:
   the ground truth from the game — what it does when a resolution change is
   requested (a settings screen? a title option? a window resize?) — to confirm
   the Fit path covers it.
3. **Startup is slow on `/mnt/DATA`** — environmental: `fuseblk` (NTFS-3G), not
   CPU and not a code defect (`docs/audits/perf-hotspots.md` #6).

## 4. Rendering / layers

* **R8b (P1)** `hasImage = true` must raise `TVPLayerCannotHaveImage` when the
  layer's type cannot carry an image (`LayerIntf.cpp:2489-2493`). The three
  non-image types are still collapsed into `BlendMode::SourceOver`
  (`crates/compat/src/natives/visual/layer/blend.rs:105`), and `LayerState` has
  no `can_have_image`. Needs L2 state plus the type mapping.
* **R9 (P1)** the steady-state projection always draws the main image and never
  reads `Layer.face` (`crates/engine/src/projection.rs:386-397`), so a `dfMask`
  or `dfProvince` layer is only faithful during a transition. `RenderNode` has
  no face field.
* **R10 (P1)** there is no per-layer affine field: `RenderNode.transform` is
  never populated (`projection.rs:412-431`), so a layer transform set without a
  repaint is lost. `AffineLayer` bakes rotation/zoom into the main image via
  `onPaint → affineCopy`.
* **Auto-path error text (C13, P2)** the trailing-delimiter rule *is* enforced
  (`crates/compat/src/natives/storages.rs:51-63`) but the text is
  `"path must end with '/', '\\' or '>'; got …"`; the reference raises
  `TVPMissingPathDelimiterAtLast` = `"Use path delimiter '>' or '/'"` —
  script-observable, so it matters.

## 5. Dialogs and popups

The reference does **not** use a native OS menu for a popup. `MenuItem.popup`
always calls `TVPShowPopMenu` and returns `1` (`MenuItemImpl.cpp:309-315`; the
Win32 `TrackPopupMenu` body is commented out), and `TVPShowPopMenu` pushes an
engine-drawn UI form into the main scene (`InGameMenuForm.cpp:115`), whose rows
the engine builds and whose activation schedules the form's removal and then
fires the item's `onClick`. So the faithful implementation draws the menu
itself; a native popup is a deliberate divergence in appearance, not a port.

Decision: implement the reference's own form. Where the platform seam goes, keep
it behind the L2 menu state so a native backend can be added later without
touching L3 — but no native backend is built now:

* **The in-game system menu is not drawn at all, and that is a compat blocker,
  not an appearance divergence.** `MenuItem.popup`
  (`crates/compat/src/natives/menu_item.rs:786`) keeps the tree in `MenuState`
  and returns the reference success value without drawing, so a game that puts
  language/settings behind a popup leaves the player stuck with no way to
  choose. The reference builds engine-drawn rows from the same tree
  (`TVPShowPopMenu` → `TVPInGameMenuForm`,
  `reference/cpp/core/environ/ui/InGameMenuForm.cpp:14-115`,
  `GameMainMenu.cpp:99`), then removes the form and fires the item's `onClick`.
  Implementing that form — rows from `MenuState`, hit-test, activation →
  remove + `onClick` — is the fix and is the largest remaining blocker for such
  titles.

* **Linux native menus/dialogs are unsupported and deferred.** The XDG Desktop
  Portal has no menu or popup interface, so there is nothing to call; the
  toolkit route (GTK popover) wants to own the main loop, which conflicts with
  the window and render loop. A portal *is* the right backend for
  `Storages.selectFile`, which is a separate item.
* **Android native popups** are a possible later backend (an Activity-hosted
  popup), not part of this change.
* The reference's dialog surface — the real `WIN32Dialog` API (`Header`, `Items`,
  `Bitmap`, `Blob`, `SolidBrush`, `getOctetAddress`, `getStringAddress`,
  `getStringFromAddress`, `initCommonControlsEx`, the instance API); only the
  class ctor and `messageBox` are registered
  (`crates/compat/src/natives/plugins/win32_dialog/mod.rs:256-280`) — is still
  open, and has the same request → form → result shape as the popup.
* The monitor dictionary's `name` string (MONITORINFOEX `szDevice`) that the host
  monitor tuple cannot supply (`crates/compat/src/natives/plugins/window_ex/system.rs`).

## 6. Core natives

Two members still differ from the reference; a member behaves like the
reference or raises because the reference raises.

* **C6** `Font.rasterizer = n` raises for `n != 0`; the reference ignores an
  out-of-range value (`LayerBitmapImpl.cpp:94-101`).
* **C11** `Font.getList(flags)` accepts `flags` but ignores it; the reference
  passes it to `GetFontList` (`LayerIntf.cpp:11977`).
* **C14 (coverage)** `scripts/native_parity.py:59` still checks only
  `reference/cpp/core/**`; the plugin surface is machine-checked by
  `scripts/plugin_parity.py`, but plugin *behaviour* rests on the audits alone.
  `Mouse`/`Key` are in neither parity map (see §7).

`Font.doUserSelect` now returns `0` after checking its four arguments, matching
`LayerIntf.cpp:11931-11956`; it was the C5 finding.

## 7. Input / audio / video

* **M3 (P1)** `onCloseQuery` is registered as a no-op
  (`crates/compat/src/natives/visual/window.rs:1966`) and
  `PlatformEvent::CloseRequested` exits outright (`crates/app/src/runner.rs:472`),
  so the reference's `OnCloseQueryCalled` **veto** (`TVPWindow.cpp:790-880`)
  cannot happen.
* **M5 (P2)** `getVisBuffer` reads the decoded source, not the device ring
  (`crates/compat/src/natives/sound.rs:1720-1765`); a positive `sampleAhead` past
  the decoded end yields silence where the reference reads the ring. The plugin
  default `ahead = 0` is exact.
* **M5b (D32)** `set_disc_audio_decoder` is a no-op seam that is still exported
  (`crates/compat/src/lib.rs:65`); the disc classes are inert. Delete the
  function and the re-export.
* **M8 (P1)** `Layer.openMovie`/`startMovie` raise where the reference logs and
  returns void (`layerExMovie.cpp:160-171`), so a `try/catch` observes a
  different outcome (`crates/compat/src/natives/plugins/layer_ex/movie.rs:126-155`).
* **M9 (P2)** window-level `onMouseEnter`/`onMouseLeave`/`onDoubleClick` are
  synthesized although the reference only declares them (`WindowIntf.h:465-485`);
  `onMouseDown` does not first run `PrimaryMouseMove` and `onMouseUp` does not
  re-check the layer under the cursor (`LayerManager.cpp:374-420`); the `Mouse`
  and `Key` native classes are port-only and appear in neither parity map; and
  `PlayStatus` lacks `"ready"` (`VideoOvlIntf.cpp:73,84`).
* **M9b (P2)** `Window.useMouseKey` is stored and never acted on. While it is on,
  the reference's `InternalKeyDown` turns `VK_RETURN`/`VK_SPACE`/`VK_ESCAPE`/pad
  keys into synthetic mouse events (`TVPWindow.cpp:648-707`).
* **M9c (P2)** `Window.postInputEvent` dispatches to the first window and its
  focused layer instead of `this` window (the reference's `_form`): correct for
  single-window titles, wrong for multi-window ones. The other stub residuals
  (`D3DAdaptor` capture, the `MenuItem.popup` surface, the CDDA constructor
  argument convention) are in `docs/audits/stub-inventory.md` §Remaining.
* **§6 input imprecisions** (documented, each needs a decision or a source):
  click/double-click timing (30 frames, double on the second **release** where
  Windows fires it on the second press); `onTouchScaling`/`onTouchRotate`
  registered but never synthesized (no reference recognizer posts them);
  layer-native `action` forwarding (only the window path forwards, because both
  would double-fire); window `onMouseEnter`; `onMouseDown` → `PrimaryMouseMove`.
* **IME / on-screen keyboard is platform-level and incomplete.** Linux forwards
  `Ime::Commit → onKeyPress` (split per character), but there is no xkb/keymap
  or dead-key handling beyond what winit provides, and the reference's
  `Window.imeMode`/`Layer.imeMode` select an IME mode the port stores but never
  acts on. **Android ignores text input entirely**
  (`crates/android/src/input.rs:45-47`) and never requests the soft keyboard, so
  a title that asks the player to type a name has no way to open the on-screen
  keyboard. Showing the Android IME (and honouring `imeMode`) is the platform
  piece; it is **not** the cause of the `keybinder.tjs` startup failure (that is
  the quickmenu `DialogLayer.names` item in §5).

## 8. File system / XP3

* **F3/F4 (P1)** two residuals: the port's "current path" is the loose app
  directory rather than the project archive (`SysInitImpl.cpp:160-165`), so a
  loose file wins over the project-archive entry; and `find` additionally
  searches **every** mounted archive after the auto paths
  (`crates/engine/src/storage.rs:516-521`) where the reference searches only the
  current folder then the auto-path table (`StorageIntf.cpp:1060-1105`).
  Auto-path on-demand opening itself has landed.
* **F6 (P1)** `Storages.getFullPath`/`getPlacedPath` return a normalized relative
  name or `arc>entry`, not the reference's full `file://domain/path`
  (`crates/compat/src/natives/storages.rs:265-286`).
* **F8–F14 (P2/P3)** `Storages.getLocalName` on a miss returns the input (the
  reference's file-media path needs an oracle decision); the wrong
  `addAutoPath` text (§4); `isExistentStorageNoSearchNoNormalize` is
  case-sensitive on the host (Windows is not); host write semantics
  un-investigated.

## 9. KAG parser / plugins

* **B.4-7 (Ex `taglist` shape)** unverifiable from the tree: only
  `reference/cpp/plugins/KAGParser/readme.txt:45-50` describes it; the plugin C++
  is not vendored.
* **PN2 (P1)** the `xp3_filter` buffer is a TJS `Array` carrying `ptr`/`count`;
  the reference's `CBinaryAccessor` adds the `ptr` cursor inside a native
  `[]`/`OperationByNum` access the C ABI cannot expose
  (`crates/compat/src/natives/plugins/xp3_filter/mod.rs:44-58`).
* **PN3 (oracle)** the vendored `jsonPlugin.cpp` is a self-labelled stub, so the
  port implements real JSON semantics; **record that decision** outside the code
  comments (there is no docs entry for it).
* **PN5 (P2)** `dirlist` does not apply the reference's `S_IFREG | S_IFDIR`
  filter (`dirlist.cpp:42`) because the delegated `Storages.dirlist` exposes no
  file-type field.
* **PN7 (P2)** the `psd://`/`psb://` media seams are not registered by the
  compat storage namespace.
* **PN8 (P2)** `windowEx`'s `ncht*` are read-only getters where the reference
  registers writable data members (`windowEx.cpp:935-960`); and
  `scripts/plugin_parity.py` cannot see post-registration globals
  (`global.X = …`), so `getDirList`, `drawFFTGraph`, `FILE_ATTRIBUTE_*` and
  `ShortCutInitial*` have no parity coverage — 100% parity is not coverage.
* **PN10 (P0, deferred)** the real `WIN32Dialog` surface — see §5.

## 10. L3 blockers

* `Window.registerMessageReceiver` records the receiver but **nothing dispatches
  messages** (`crates/compat/src/natives/visual/window.rs:56-58,1703-1729`; the
  registry is read only by tests). Reference `WindowImpl.cpp:2150-2163` forwards
  to the platform.
* `windowEx` dynamic events (`onMinimize`/`onResizing`/…) are matched by name but
  never raised: there is no native window-message dispatcher on Linux
  (`crates/compat/src/natives/plugins/window_ex/window.rs:100-110`).
* **Hatch `Percent50` paints 25%** where the reference's checkerboard paints
  `(x + y) % 2 == 0`, i.e. 50% (`crates/engine/src/raster.rs:209-247` vs
  `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:681-686`). Every
  other hatch style matches.

## 11. Android

The Android target is **never built or tested on the development host**: no NDK
is installed and none may be installed. Android-only code is written and
reviewed here and compiled and run on a machine that has one; anything not
compiled is documented as unverified, never described as working. The host bar is
`cargo check --workspace` staying green plus the `platform-android` host-feature
steps and `scripts/check-android-target.py` in the gate.

A separate composition root (`crates/android`) owns `android_main`, the
`GameActivity` loop and the JNI reads; the window, AAudio output and host-testable
input translation are real, MediaCodec is compile-checked and never run.

Open:

* `tjs/build.rs` is not target-aware (`docs/android.md` §2).
* Audio focus / lifecycle handling.
* On-device validation of everything (D36): startup, decode, render, MediaCodec,
  `memfd`, touch mapping, surface recreation, and **font resolution** (the
  system directories are populated and a requested face loads) — captured in
  `<filesDir>/krkr.log`. No release without this run.
* Surface recreation restarts the game (`docs/audits/android-jni-review.md` F5).
* No drift guard between the Kotlin option constants (`RuntimeSettings.kt`) and
  the Rust ones (`api::config`) — they agree today by hand.
* The drag-slop threshold is a fixed 16 px where the reference scales by DPI
  (documented divergence).
* `docs/runtime-config.md` still carries a stale snippet (F10 in the JNI audit).
* The launcher's whole UI is hardcoded English; `strings.xml` holds only
  `app_name`, and no `values-zh` exists (`docs/audits/android-ui-review.md` F5).

## 1.1 Current priority

1. **L2 owns the frame** (`D26`/`D27`) — **landed (frame slice).**
   `Engine::step(dt_ms, hooks)` / `Engine::frame(now_ms, hooks)` own the clock,
   timers, projection and render; `GameFrame::before_render(now_ms, dt_ms)` is
   the single upward hook. `app::Host` wraps the engine, `GameHost` is the
   hook, and the timer tests drive virtual time. Remaining: move the mixer,
   video and transition pumps out of the hook, add `Engine::run`, and collapse
   the timer surface to D27's four methods (`07` Phase 9).
2. **The pixel-translation layer** (`D39`/`D40`) — **mapping landed.** A pure
   L2 `PixelTranslation` maps VM pixels (what scripts see) to device pixels
   (what the framebuffer uses), applies Fit-with-letterbox during projection,
   and input is inverse-mapped before the VM tick. Phase 9a (the `ScalingMode`/
   `PresentationScaling`/`--scaling`/`ViewTransform`/`native_size` removal)
   has landed.

3. **Explicit VM resolution (next)** — the launcher can set the game's VM
   size (`--vm-size WxH`, and the display/window size) so the game runs at
   that resolution instead of its declared fixed size. The existing Fit
   translation maps the forced VM surface to the window; scripts see the forced
   size (`System.screenWidth`, window). This requires the scene window and
   primary layer to follow the forced size and script window-size changes to be
   held at it. Default remains the game's declared size.

Open sub-items are tracked there and in `07`; the remaining mixer/video/
transition pumps above still apply. Render-resolution decoupling (keep VM,
CPU-raster at a higher fixed resolution) is documented as a deferred idea in
`07`, not implemented; the earlier raw-buffer/plugin compatibility concern is
why the VM-size override is the near-term path.

## 12. Deferred design work

* **TJS VM Rust port** — deferred (`08 §10.7`).
* **`streams.cpp`: thin C++ adapter over a Rust implementation** — deferred. It
  is the only port-authored C++ besides the ABI boundary (794 lines), and every
  memory-safety defect in the save path was in it.
* **Windowed audio on a real device** — the cpal backend is real and wired
  (`crates/platform/platform/src/linux.rs`, `crates/app/src/audio.rs`); owed is
  the device validation.
* **A GPU text path** — `RenderContent::Glyphs` is rejected by both renderers
  (`render/src/cpu.rs:240`, `render/src/vulkan/draw.rs:233`),
  `supports_glyph_atlas` is false on every backend and read by nothing, and
  `RenderTarget`/`read_render_target` are wired nowhere. Text is rasterized on the
  CPU into the layer bitmap, which is also what `getPixel`/`saveLayerImage` read
  back — that composite is the observable representation (`D34`), so this is not
  a rasterizer swap. Recorded in `docs/optimization.md`.
* **An L3 render-target surface** — L1 exposes
  `create_render_target`/`read_render_target`, but neither `engine` nor `compat`
  plumbs them, so `D3DAdaptor.captureCanvas` cannot copy a rendered texture into
  its layer and the player's `renderToD3DAdaptor` path is unwired. The same
  missing readback the GPU text path needs.
* **Zero-copy image presentation** — a later stage, not a repair: uploads go
  through a host-visible staging buffer with no `dmabuf`/`AHardwareBuffer` path
  anywhere. Expected gain ~0–5% for sprites and ~10–25% for movie playback.
* **C++ `-O2 -flto` for the TJS2 VM** — measured, deferred. It wins on paper
  (compile 89.5%, exec 92.5% of `-O2` on the host harness in `scripts/perf/`),
  but the objects become LLVM bitcode and the Android link step is the only
  place it can be validated, so it needs a device A/B before adoption. `-O3`
  was measured and rejected (exec 102.6% of `-O2`), as were
  `-fvisibility=hidden`/`-fno-semantic-interposition` (flat) and `mimalloc`
  (~0.08 allocations per op). `TJS2_OPT`/`TJS2_EXTRA_CXXFLAGS` keep every level
  switchable (`docs/performance.md` §7).

## 13. Verified — do not re-open

Checked and found *correct* (or correctly inert) by the audits; changing any of
these needs new evidence, not a re-read.

* **R6** `Font.getTextWidth`/`getTextHeight` are **not** rotated; only `getEsc*`
  multiply by `cos/sin(angle·π/1800)`. The real divergence there
  (`measure_text` height) is fixed.
* **R7** `System.stayOnTop` is **not** a reference member; the script writes a
  dynamic member.
* **M4** the reference compiles the MIDI/CDDA classes out; both are inert
  arity-checked no-ops with per-class property values pinned (`2af9b9b`).
* **F7** `exists` reporting directories matches `TVPIsExistentStorage`; only the
  review doc is stale (§4).
* **F1** a scanned archive is skipped with a warning and an explicit `--archive`
  entry is a typed error — a deliberate contract, documented in
  `docs/migration/09-game-loading.md:71-75`.
* **PN9** reference-stub members stay as they are (`Point::contains`,
  `setMaxTextureSize`, `TextRender.newline`/`setKeyWait`, the `fstat` no-op
  returns).
* The class-name constructor member exists on every registered class; `HMENU` is
  `0` (`MenuItemImpl.cpp:193-196`); `MenuState` is a full logical menu tree.
* `layer_ex` movie playback is wired (`crates/app/src/game.rs:91`); `LayerExFont`
  leading metrics and the linear gradient are implemented; `DrawDevice.interface`
  and `DrawDeviceForSteam.interface` report a stable synthetic pointer.
* `VideoOverlay` frames are composited into `layer1`/`layer2` and the mixing
  layer; `vomOverlay` is deliberately not emulated.
* Plugin/native parity is 100% with empty allowlists: `native_parity` reports
  0 missing and **3 intentional extras** (`KAGParser.multiLineTagEnabled`,
  `paramMacros`, `PhaseVocoder.__TvpPhaseVocoder`; `docs/native_parity.md`).

## 14. Error handling

`scripts/check-error-handling.sh` enforces §4 mechanically (bash + python3, no
compiler) and runs in the gate, right after the stateless guard.
`scripts/error_handling_allow.txt` holds the sites whose failure is genuinely
ignorable, each with a reason; the guard fails on anything else, and reports a
stale entry when code moves. The audit is `docs/audits/error-handling.md`.

Remaining:

* **`AudioSpec::channels` is `u16`** where the reference keeps a 32-bit
  `tTVPWaveFormat::Channels` (`reference/cpp/core/sound/WaveIntf.h:47`). A
  decoder count outside that range is now a typed error rather than a zero, but
  the narrowing is the divergence; widening the field is an `api` change.
* The scan cannot tell a `Result` receiver from an `Option` one, so it
  over-reports by design. A site that needs a decision and has no allowlist
  entry fails the gate rather than being accepted silently.

## 15. Audit reports

| topic | report |
|---|---|
| Focus model, triggers, plan | `docs/audits/focus-system.md` |
| Focus on invalidate/destroy; managers per root | `docs/audits/focus-invalidate-events.md` |
| Stubs and reference no-ops | `docs/audits/stub-inventory.md` |
| Object write dispatch | `docs/audits/object-write-dispatch.md` |
| Save crash | `docs/audits/save-crash.md` |
| Memory footprint | `docs/audits/memory-footprint.md` |
| Unsafe inventory | `docs/audits/unsafe-inventory.md` |
| Image-load cost | `docs/audits/image-load-cost.md` |
| Perf hotspots | `docs/audits/perf-hotspots.md` |
| Video overlay | `docs/audits/video-overlay.md` |
| Android launcher UI | `docs/audits/android-ui-review.md` |
| Android JNI seam | `docs/audits/android-jni-review.md` |
| Android storage/lifetime | `docs/audits/android-storage-review.md` |
| Error handling, fallbacks and panics | `docs/audits/error-handling.md` |
| The menu popup surface | `docs/audits/menu-popup.md` |

## 16. Next minor version (v2.1) — performance and bug fixes

This major version is about behaviour and architecture. These are deliberately
deferred and do not block v2.

### A scene transition takes about two seconds in release

A release run logged 60 `load_layer_image` calls inside one transition,
13:00:57.620 → 13:00:59.558 — one load every ~32 ms, so ~1.9 s end to end, all on
the VM thread. The same name appears repeatedly: `FRM_02C11` is loaded into about
thirty different layers (209, 215, 221, … 374). Either the decoded-image cache is
not consulted before decoding, or it is not retaining these entries.

Work through these in order:

1. **Is the decoded-image cache actually consulted?** `Scene::cached_decoded_image`
   exists and `System.graphicCacheLimit` drives its bound, but a limit of 0
disables it and nothing asserts a hit on a real game's load path. Count hits and
misses for the sequence above: the repeated `FRM_02C11` must be a hit after the
first decode, and every miss must be explainable (a different key, eviction, or
a limit of 0).
2. **The decode is synchronous on the VM thread** (`docs/audits/image-load-cost.md`),
   so a scene change blocks the frame until every image is decoded. A worker pool
   that hands results back to the VM thread — the pattern the sound decode-ahead
   worker already uses — would overlap them. Measure the payload first: TLG
   1280×403 was 15.3 ms in release, so 60 loads is the arithmetic above.
3. **Speculative prefetch**: scan the scenario scripts for image-name literals
   and enqueue decodes before the script asks for them, into a bounded cache.
4. **A per-frame budget**: a transition that must load many images should yield
   and continue across frames instead of stalling one.

Keep the evidence: `docs/audits/image-load-cost.md` (the 6.77 s frame was the sum
of synchronous decodes) and `docs/audits/perf-hotspots.md`.

### Other candidates

* The two real-game reports in §3 that still need a repro from the field: the
  save crash (the serializer recursion plus a missing backtrace) and the
  resolution change.
* Dirty-rectangle bitmap uploads and the per-frame fence wait (§12).
* Bounded GPU glyph-atlas cells (§12).

### The address-safety guard is a trigger, not a proof

The `pointer-flow`/`pointer-compare`/`char-signedness`/`addr-narrow` rules in
`scripts/check-error-handling.sh` are a one-function-body taint over a
name-based interprocedural set. They do not catch a pointer laundered through a
`usize`-returning helper with no address-suggesting name, taint crossing a
struct field or a closure, address-keyed state with neither a cast nor a
comparison, or a pointer round-tripped through script state. `D37`
(`docs/migration/README.md` §3) and `docs/audits/pointer-handles.md` remain the
review backstop, and the audit's §9 follow-ups (rank 3 address-keyed maps, the
`PhaseVocoder` token) are open.

### Device verification owed

Each of these needs a real KiriKiri title on a real device, which is the only
place they can be confirmed (`D36`):

* the `Layer.drawPolygon`/`drawPath` native-instance fix (the aarch64 black
  screen) — confirm the scene transition past "start game";
* the slow-frame attribution line — confirm which field carries the ~558 ms
  stall and that `unaccounted` closes the budget;
* one export per title for the four titles whose failure reason was never
  captured, now that the log keeps six rotations and the state file is
  included;
* whether any shipped script compares `Window.HWND`/`Bitmap.buffer`/
  `Layer.*ImageBuffer`/`interface` numerically (the rank 2 getters in the
  pointer audit).

### Start-up is not measurable on a device

Before optimising anything, make the cost visible where it happens. Android's
logger defaults to `info` (`crates/android/src/lib.rs`), and `RUST_LOG` cannot be
set on a device, so the per-image `debug` lines a desktop run shows never reach
`krkr.log`. The startup path logs no timing at all: at info level it prints the
game directory and little else, so "it took a long time to load" cannot be
attributed to reading, decoding, compiling scripts or the first frame.

Two small pieces, neither of them a micro-optimisation:

* **Aggregate timing at info level**: images loaded, bytes read, decode
  milliseconds, scripts compiled, first frame presented — one line per phase, so
  a release APK reports its own start-up cost without debug logging.
* **A log-level option**: the launcher owns configuration on Android, so the
  level belongs with the other options rather than in an environment variable
  that cannot be set on the device. It also needs to be re-readable per launch,
  since the logger is installed once per process.

### Decoding on the GPU — checked, and not the lever

Recorded so it is not re-opened without new evidence.

* **MediaCodec decodes to memory, not to a surface.** Both decoders are
  configured with no surface (`mediacodec.rs` `configure(&track.format, None,
  Decoder)`), so the output is a YUV byte buffer that the port converts on the
  CPU (`yuv420_to_rgba`, `media/src/yuv.rs`) and then uploads. A hardware
  decoder may sit behind that, but nothing downstream of it is GPU-side. The
  surface path (`Some(surface)` → a Vulkan image through `AHardwareBuffer`) is
  the zero-copy work in §12, and it is blocked today because
  `COLOR_FormatYUV420Flexible` needs `AMediaCodec_getOutputImage`, which `ndk`
  does not expose. The desktop FFmpeg path is CPU-only as well — no `hwaccel`,
  no `hw_device`, no `get_format`.
* **TLG is not a GPU decode target.** Both formats are entropy-coded bit
  streams with serial dependencies: TLG5 is LZSS over a sliding window shared
  across the image, TLG6 is Golomb-Rice/RLE with a per-block method map, then
  predictors and colour filters. The parallel stages are the cheap ones, the
  entropy decode dominates, and the result has to come back to CPU memory
  because the layer pixels are what `getPixel`/`saveLayerImage` observe (D34).
  The renderer also has no compute pipeline at all. The lever is scheduling -
  the cache hit above and decoding independent images on a worker pool - not a
  compute shader.

### Guard-recorded debt

`scripts/check-layering.sh` baselines two real violations so it can forbid new
ones. Both are tracked work, not accepted practice, and the allowlist entries
carry a `debt:` marker so clearing one is visible:

* **25 `std::fs` uses in compat source, across 6 files** (`l3-platform`).
  AGENTS §2 keeps platform code in L1; these should route through the L1 `fs`
abstraction. The guard excludes `crates/compat/tests/**`, where `std::fs` is
legitimate for fixtures.
* **90 `unsafe` blocks with no `// SAFETY:` note, across 19 files**
  (`unsafe-no-safety-note`). AGENTS §5 requires a note per block; the sites are
already inventoried in `docs/audits/unsafe-inventory.md` §2.6. Each note needs
a real safety argument, so this is review work, not a bulk edit. Baselined
per file, so a new file or a new rule fails the gate. The three FFI-heaviest
files (105 blocks: `visual/window.rs`, `tjs/src/lib.rs`, the Vulkan files) are
reviewed and cleared, and that review found two real defects — a `&mut` held
across a script eval in `window_ctor`, and 22 call-site `unsafe` blocks the TJS
value helpers forced.

## 17. Next major version — native plugin coverage

The next major version brings the unported native plugins. 36 of the 53 plugin
names real games ship have no emulation (`plugins/INVENTORY.md`; the DLLs are
gitignored and are not committed), so each of those takes the null-plugin path
(D14) when a game links it: every member accepted, every call void, one
warning. Two are already visible in a device log as null plugins —
`PackinOne.dll` and `menu.dll` — and `yuzuex.dll` is Yuzusoft's own extension,
which matters for the titles that still fail to start.

Workflow, per plugin:

1. Recover the script-visible surface (registered class and member names,
   argument arity, the installed global) from the DLL. The pipeline and its
   measured fidelity live in `plugins/analysis/`, validated against
   `layerExDraw.dll`, whose reference C++ we do have.
2. Recover semantics for the members that need them, by decompilation.
3. Emulate it in `crates/compat/src/natives/plugins/<name>/`, register the name
   in `BUILTIN_PLUGINS`, and add behaviour tests.

Priority is how many games ship it times the effort, ranked in
`plugins/analysis/PRIORITY.md`: `krmovie.dll` (5 versions), `yuzuex.dll` (4),
`PackinOne.dll`, `menu.dll`, `AlphaMovie.dll`, `k2compat.dll`, `kagexopt.dll`,
`win32ole.dll`, `wfBasicEffect.dll`, `wfTypicalDSP.dll`,
`motionplayer_nod3d.dll`, `DrawDeviceD2D.dll`, then the single-version names.

**A citation rule is owed for these.** Every behaviour assertion must cite the
reference (D30), but an unported plugin has no `reference/cpp` source: the DLL
is the oracle. The proposed form — the DLL path, its content hash, and the
decompiled function or address — is recorded in `plugins/analysis/README.md`
and needs a decision entry before the first such plugin lands, because a test
that cannot cite its evidence is not evidence.

**Parity extractor coverage (`docs/audits/propset-parity.md`).** Both parity
scripts now read class-level `PropSet` registrations, namespace attachments
and script globals, so the `Window.menu` class of gap is caught. Two
extractor gaps remain, neither a missing port member:

* **False `name` member from a macro definition.**
  `native_parity._preprocess_cpp` does not blank `#define` bodies, so
  `#define TVP_REGISTER_PTDD_ENUM(name)` contributes a member literally named
  `name` to `BasicDrawDevice`/`PassThroughDrawDevice`; the runtime comparison
  lists it as missing. Fix: blank `#define` bodies in the core reader the way
  the plugin reader already does, then review the (only-shrinking) diff.
* **Global free functions are not in the runtime dump.**
  `runtime_surface.rs` walks class objects and instances, not the global
  object's free functions, so `--runtime` always reports
  `getDirList`/`drawFFTGraph` as missing. Fix: enumerate global function
  members in the dump, or teach the runtime comparison that `__global__` is
  not represented. A host-only change; no script behaviour is involved.

## 18. Handoff state after `android-v2.0.8-alpha`

`main` is at `636f5c3`, tagged `android-v2.0.8-alpha` (run `38382613439`, the
first build with the current action majors and JDK 21). The gate is green on
this tree: fmt, the four guards, clippy, the workspace suite, `platform-linux`,
`platform-android`, the per-target check, the surface lock (2515 members) and
both parity reports (640/974 members, 0 missing). Everything below says whether
it is verified or not; nothing in the release is device-confirmed yet.

### 18.1 First task: verify v2.0.8 on a device

The release fixes four separate causes of "cannot open the game". Install it,
set the log level to `debug`, run each title, and send one export — it should
now cover several launches.

| title | previous failure | what shipped | expect |
|---|---|---|---|
| 宠物志愿 | `Config.tjs が見つかりません` | the auto-path project fix (18.2) | no `storage miss "Config.tjs"`; startup gets past `kag.process("first.ks")` (`system/Initialize.tjs:280`) |
| 魔女的夜宴 | same | same | same |
| 我的眼睛能看光光 | `Bitmap: storage not found: FRM_0303l` | probably the same fix | the frame resolves, or a different error |
| 千恋万花 | `syntax error` (TJS2100 bytecode) | precompiled-bytecode loading | `script read 'startup.tjs' … encoding=precompiled bytecode`, no syntax error |
| 绝对高潮 | `Member "menu" does not exist` | `Window.menu` | the property resolves |

When a title still refuses, the log now says why: `script failure '<name>'`
prints the source around the failing line, and a storage miss prints the entry
count, the index-section breakdown and the nearest entry names
(`docs/audits/xp3-lookup.md`, `docs/audits/startup-diagnostics.md`).

Still owed from the previous release: the slow-frame line's attribution of the
~558 ms stall (`platform_wait` vs `unaccounted`, see §16), and whether the
`FE FE` mode 1/2 offset choice matches a real game-written container.

### 18.2 The auto-path fix, and why it mattered

`Storages.addAutoPath("system/")` was kept as a **disk** path, so a bare
`Config.tjs` was searched on disk and at each archive's *root*. The reference
normalises a relative auto path against the current directory, which is the
**project path** — and when that path is an archive the engine appends `>`
(`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`), so the auto path
becomes `data.xp3>system/`; `TVPRebuildAutoPathTable`
(`reference/cpp/core/base/StorageIntf.cpp:959-1052`) then indexes that prefix's
**direct children** by base name, and `TVPGetPlacedPath` (`:1061-1105`) resolves
a request by its **base name**, caching hits and never caching misses.

Implemented: the auto-path table and its invalidations, the project as an
explicit input from the composition root (never mount order — D35 sorts
case-insensitively, so `bgimage.xp3` mounts before `data.xp3`), and the miss
diagnostic. The rule for a directory launch lives in `crates/app/src/game.rs`:
the conventional `data.xp3`, else the archive holding the boot scripts, else the
directory. Host check available: `/mnt/DATA/Games/Others/test2/data.xp3` is
宠物志愿's archive (1384 entries, one zlib index block, contains
`system/config.tjs`) — `Config.tjs` must resolve there through an auto path.

### 18.3 Open work, in priority order

1. **`PackinOne.dll`** — top of the plugin queue by *observed usage* (the only
   unemulated plugin real games actually link, 4 of 28 launches, and it
   correlates with a failing title). Surface and priority: `plugins/analysis/`
   (gitignored, with the DLLs). Its strings include `ProxyStorageMap`,
   `loadDataPack`, `PackinOneList`; two registration blocks remain unresolved.
   **If it is a storage proxy, a surface-only emulation will not fix anything —
   it needs behaviour** (18.4, Tier 2).
2. **Other linked but unported plugins**: `krglhwebp.dll`, `clipboardEx.dll`
   (seen in a device log), plus the inventory's 35 unemulated names.
3. **Delete `unsafe impl Send + Sync for Tjs2Engine`** — unsound as a public API
   (two engines race process-global VM state) and nothing in the tree was found
   to require the bounds, so remove them and let the compiler name any caller.
   `docs/vm-defects.md` GLO-1 / LIFE-2.
4. **Image decode scheduling** — the measured 6.77 s frame and ~1.9 s transition
   are sums of synchronous decodes. The plan is prefetch, cache consultation and
   a bounded worker pool; `fork_union`/`rayon`/std threads are the candidates,
   to be chosen on a measurement. This is where the remaining seconds are.
5. **`unsafe` SAFETY-note debt** — 89 findings across 19 files, baselined per
   file by `scripts/check-layering.sh` so new debt fails the gate (§16).
6. **Parity blind spots left** — `global.X = …` in script strings, member names
   built at runtime, and two *false* reference members
   (`BasicDrawDevice.name`/`PassThroughDrawDevice.name`) that the extractor
   invents from a `#define` body. `docs/audits/propset-parity.md` §6.
7. **`BLK-3`'s open question** — whether the shipped engine shares the reference
   snapshot's incomplete bytecode export table. Settle with a game that uses
   `Scripts.compileStorage` on a `+=`/`++` statement, or a reference build whose
   translation table differs (`docs/vm-defects.md`).
8. **Deferred by decision**: C++ `-O2 -flto` (needs a device A/B, §12), and the
   GPU text path and render-target readback (§12).

### 18.4 Plugin coverage plan (the next major version)

Ranked by **observed links first**, shipped copies second, effort third
(`plugins/analysis/PRIORITY.md`). Four tiers, because they need different work:

1. **Surface only** — register the extracted member names so calls resolve and
   take the null-plugin behaviour; mechanical, and it moves titles past
   `Member "x" does not exist`.
2. **Behaviour by decompilation** — for members a game depends on where void is
   not enough (a storage proxy is the archetype). Bounded: the pipeline records
   each method's implementation pointer.
3. **Engine features** — the object-interface plugins are not plugin work:
   `krmovie`/`yuzuex` need video/audio backends in L1/L2.
4. **Windows-bound** — `win32ole` (real COM), `win32dialog`, `drawDeviceD3D`:
   emulate the API or declare out of scope with evidence.

The lever that keeps this measured rather than guessed: **our VM sees every
native call**, so logging calls to unemulated members turns a device log into a
work list ranked by real use — the same signal that identified `PackinOne`.

### 18.5 Where the evidence lives

| artifact | what it holds |
|---|---|
| `docs/vm-defects.md` | the VM/reference defects a rewrite must not rediscover |
| `docs/audits/xp3-lookup.md` | the auto-path model, the fix, the tests |
| `docs/audits/startup-diagnostics.md` | the failure-source excerpt and its guarantee |
| `docs/audits/bytecode-scripts.md` | `TJS2100` loading, the loader guard, the writer defect |
| `docs/audits/propset-parity.md`, `layering-guard.md`, `house-style-guard.md`, `test-fidelity.md` | the guards and the blind spots they close |
| `docs/audits/unsafe-inventory.md` | the unsafe review, incl. the two real defects it found |
| `plugins/INVENTORY.md`, `plugins/analysis/` | the DLL queue, surfaces and priorities (gitignored) |
| `krkr-rs-logs*.txt` (gitignored) | device logs; the failure fingerprints are in §3 |

### 18.6 Method notes worth carrying forward

* **Read the reference before implementing.** The `Config.tjs` bug was solved in
  ten minutes once `TVPGetPlacedPath`/`TVPRebuildAutoPathTable` were open; before
  that, three different mechanisms were reasoned out and all three were wrong.
* **Reuse the tools already in the tree.** Three hand-written XP3 parsers were
  wrong in three different ways (magic, offset, layout) while
  `scripts/extract_xp3.py` already read the format correctly.
* **A count is not a name set.** "Both engines see 1384 entries" hid a name-level
  lookup bug; assert that a known name resolves, per index shape.
* **Verify tags and versions with `git ls-remote`**, never from memory — the
  current action majors were newer than the deprecation notice implied.
* **Releases are metered**: tag only on the user's go-ahead (AGENTS §0).
* **A subagent's edit scope does not always widen for a running session**; apply
  its verified patch locally instead, and **verify receipt** of load-bearing
  steering messages — one was silently dropped this session.

## 19. The v2.0.8 device run (log 5) and the fixes on this branch

`krkr-rs-logs (5).txt` is the first real-device run of `v2.0.8-alpha`. The
auto-path fix (18.2) worked — `Config.tjs` resolves and the scripts load — but
three titles then hit new blockers past it. All three are fixed here, with the
full gate green. Everything below is host-verified; none of it is device-verified
yet.

### 19.1 The failures and their root causes

| title | failure in log 5 | root cause | fix |
|---|---|---|---|
| 宠物志愿 | `Member "innerSunken" does not exist` at `MainWindow.tjs:309` | the shipped header enables `USE_OBSOLETE_FUNCTIONS` (`WindowIntf.h:23`); the port registered none of that block | register the six members |
| 绝对高潮 | same at `MainWindow.tjs:311` | same | same |
| 魔女的夜宴 | same at `MainWindow.tjs:1733`; then a doubled `saveDataLocation` | same; plus `System.personalPath`/`savedGamesPath` returned `<game>/savedata/` | register the members; return `TVPGetAppPath()` |
| 千恋万花 | `Storages.isExistentDirectory: Cannot get local name from …/setup/debug//` | the mount prefix was stripped case-sensitively, so the normalized absolute path no longer matched the on-disk `Games`/`KR` spelling | strip the prefix component-wise, ASCII-case-insensitively |

### 19.2 What landed

* **The `USE_OBSOLETE_FUNCTIONS` Window members.** `beginMove`, `setLayerPos`,
  `layerLeft`, `layerTop`, `innerSunken`, `showScrollBars`. `innerSunken` is
  stored on the window (the port already stores `focusable`/`useMouseKey`); the
  other five are the reference form's no-ops/constants (`TVPWindow.h:135-184`).
  Surface baseline: 2515 -> 2521 members.
* **`TVPNormalizeStorageName`.** Both `compat` normalizers now lowercase the
  disk/domain path (`tTVPFileMedia::NormalizeDomainName`/`NormalizePathName`,
  `StorageImpl.cpp:95-117`) and run the in-archive part through
  `NormalizeInArchiveStorageName` (`StorageIntf.cpp:558-604`). The old helper
  lowercased but did not collapse the in-archive slashes.
* **Case-insensitive mount-prefix strip.** `Storage::raw_relative_disk_name`
  compares components with `eq_ignore_ascii_case`
  (`GetLocallyAccessibleName`, `StorageImpl.cpp:301-407`), so an absolute name
  that differs from the mount point only in case resolves instead of raising
  `TVPCannotGetLocalName`.
* **`System.personalPath`/`savedGamesPath`** return `TVPGetAppPath()` (the game
  directory), matching `SystemImpl.cpp:341-359,387-397`. Only `System.dataPath`
  is `<app>/savedata/` (`ApplicationSpecialPath.h:13-19`).

### 19.3 Open after this branch

1. **The doubled `魔女的夜宴` save path is not root-caused.** The log shows
   `G//G/savedata/` and then a failed `…/savedata//savecheck` open. The
   `personalPath`/`savedGamesPath` divergence is fixed, but `exePath + <any
   property>` still doubles on the reference's own values, so no single property
   explains an expression that works there. **Needs the game's `Config.tjs`**
   (present only in the device's XP3 archives) or a TJS-level trace of the
   expression that builds `saveDataLocation`. Do not add a title-specific patch
   (D28).
2. **Device verification of all of the above (D36)** is still owed: a fresh run
   of the four titles with the log exported, and the §18.3 priority list after
   that.

### 19.4 The second round (this branch)

* **Storage resolution moved to L3.** The `file://`/`file:`/`file://.` media
  prefix is parsed at the `compat` boundary (`natives/storages.rs`
  `strip_file_media`) before any engine call; the engine namespace never sees
  it. `docs/migration/06-file-system.md` contradicted D2/D6/`02 §2.3` by
  handing TVP storage naming to L2; `docs/audits/storage-layering.md` records
  the target split (L2 = a TVP-free mount/read VFS, L3 = the name grammar and
  resolution), with the first slice staged. The `layering` check gained an
  `l2-storage-name` rule so a `file:` literal in `crates/engine/**` fails the
  gate.
* **Live directory listings.** `Storage` no longer caches directory listings;
  the case-insensitive walk and `local_name` enumerate on every call, matching
  the reference's live `opendir`. The auto-path candidate table and the
  resolved-hit table stay cached.
* **`Layer` property accessors.** `imageWidth`/`imageHeight` gained the
  reference setter (`SetImageWidth`/`SetImageHeight`, `LayerIntf.cpp:2565-2621`)
  and their getters now raise on an imageless layer; `cursorX`/`cursorY` gained
  setters; `font` lost its setter (the reference denies it,
  `LayerIntf.cpp:11339-11350`). The `imageWidth` omission was the real-game
  `system/MessageLayer.tjs:830` startup failure.
* **The accessor parity gate.** The `native-parity` check now extracts the
  reference getter/setter per `TJS_BEGIN_NATIVE_PROP_DECL` and the Rust
  `NativeInstancePropertyDef { get, set }`, and fails on any mismatch in either
  direction (`scripts/allowlists/native_parity_props_allow.txt` ratchet +
  fixtures). This is the blind spot that let the `imageWidth` bug through.

### 19.5 The third round (this branch)

* **Host facts are wired.** `System.desktopLeft/Top/Width/Height` install a
  desktop-rectangle fact from the surface size (`tTVPScreen::GetDesktop*`,
  `SystemImpl.cpp:897-935`); `System.appDataPath` is computed, not a host fact
  (`TVPGetAppDataPath` returns `TVPGetAppPath`, `SystemImpl.cpp:366-380`); the
  reference's inert clipboard is installed (`ClipboardImpl.cpp:5-19`).
* **One storage media boundary.** `crate::storage::strip_file_media` is the
  single home for the `file://.`/`file://`/`file:` prefix; every script-name
  loader, the video seam, the write-side joins, `saveRecord`/`loadRecord` and
  the log location route through it. A `file://.`-prefixed lowercased
  `System.dataPath` now resolves case-insensitively (the `test` game's
  `saveMng.dat`).
* **The gate is one uv project.** `uv run --project scripts krkr-gate gate`
  runs the whole sequence; each check is also a subcommand (`stateless`,
  `error-handling`, `house-style`, `layering`, `storage-boundary`,
  `host-facts`, `android-target`, `surface-lock`, `native-parity`,
  `plugin-parity`, `extract-xp3`).
* **New class-level checks.** `host-facts` fails a `CompatHost` provider seam
  with no production installer; `storage-boundary` fails a script-supplied
  name that reaches L2 or a host seam without `strip_file_media` in
  `crates/compat/src/**`.

## 20. Open: GPU descriptor-pool exhaustion / memory growth

**Status: open — deliberately not fixed yet; do a full audit first.**

Symptom (windowed Vulkan, a real title, after a while of play):

```
WARN app] failed to upload N bitmap(s): out of GPU memory: failed to allocate a
          texture descriptor set: A device memory allocation has failed
```

repeating every frame, alongside `audio: … frame exceeds the 250 ms playback
cap`. It appeared on the branch after `android-v2.0.9-alpha` and persists after
the two self-cycle leak fixes below.

What is known:
* `create_descriptor_pool` is `COMBINED_IMAGE_SAMPLER × 1024`, `max_sets(1024)`,
  with `FREE_DESCRIPTOR_SET`, and `texture.destroy` frees the set. So the pool
  only exhausts when more than 1024 textures are live, or a texture handle is
  dropped without `destroy` (or re-created under a key without destroying the
  old one).
* The failure means `scene.bitmaps` is not dropping, so `prune_stale_textures`
  never frees the textures. Which object still roots those bitmaps is not yet
  identified.
* Fixed so far (do not re-open): the layer registry no longer roots every layer
  (`ObjectRegistry.layers` is a non-owning handle); MenuItem / Timer /
  WaveSoundBuffer / AsyncTrigger no longer store their own `objthis`; the
  motion snapshot store evicts on `unload`/`clearCache`; the `psb://` media is
  the reference's **session-global** cache, not per-file.

What the audit must do:
1. Add live counters to the debug overlay — `scene.bitmaps`, `Host::textures`,
   the Vulkan texture/descriptor-set count, and the `ObjectRegistry` map sizes —
   then drive a real page/motion/config cycle on a title and watch which
   counter grows (and whether it ever falls).
2. Separate "a genuine retainer" from "the reference's own graphic cache
   limit". The reference bounds live graphics through `System.graphicCacheLimit`;
   the port uploads one texture per live scene bitmap with **no** such bound, so
   a scene with >1024 live bitmaps exhausts the pool even with no leak at all.
3. Only then decide: fix the retainer, or bound the live texture set the way the
   reference bounds its graphic cache (its own mechanism — not an arbitrary
   cap).

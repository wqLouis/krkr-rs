# TODO — open work

**Branch:** `migration/middle-layer-ash` · **repo:** `/mnt/DATA/Document/code/krkr-rs`
**Real game:** `/mnt/DATA/Games/Others/test` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

This file lists **only what is still open**, verified against the tree by four
read-only audits (rendering, natives/KAG/plugins, media/input/fs, infra) whose
findings are folded in below with the landing commits they cite. Behaviour that
was checked and found *not* to be a divergence lives in §13 so it is not
re-opened. History is read with `git log`.

---

## 0. Conventions

* **Subagents must not run `cargo`** (`build`/`check`/`clippy`/`test`/`run`) — parallel builds once exhausted a 16 GB tmpfs and broke the tree. They may use `rustfmt`/`cargo fmt`, run prebuilt artifacts (`target/debug/krkr`, `target/debug/deps/*`), and run the Python scripts. The main agent runs the gate.
* **Only the main agent runs `scripts/surface_lock.py --update`.**
* **Comments** (D31): describe the code as it is — no history, no "used to", no rationale, no decision numbers. Keep `// SAFETY:`, invariants, and reference citations **in tests**.
* **Tests** carry a `reference/cpp/...:line` citation above every behaviour assertion. An assertion without one is not evidence.
* **Printed/logged text must not use internal jargon** (`L1`–`L4`, decision numbers). Tests are exempt.
* **D32:** only the Android backend may be stubbed; L3 may not stub and may not approximate. `#[allow(dead_code)]` is banned (one exception: `crates/compat/tests/common/mod.rs`).
* **Traps that cost real time live in `AGENTS.md` §9** and are deliberately not duplicated here.

## 1. The gate (run in this order)

```bash
cargo fmt --all -- --check
bash scripts/check-stateless.sh                                   # D9 guard
bash scripts/check-error-handling.sh                              # AGENTS §4 guard
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast                             # all crates; never fail-fast
cargo test -p platform --features platform-linux
cargo clippy -p platform --features platform-android --all-targets -- -D warnings
cargo test -p platform --features platform-android
python3 scripts/check-android-target.py
python3 scripts/surface_lock.py --update                          # main agent only; review the diff
python3 scripts/surface_lock.py
python3 scripts/native_parity.py
python3 scripts/plugin_parity.py --no-doc
```

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
2. **The game cannot change resolution — open, needs a repro.** No audit
   defines the trigger and the game data is packed. The likely-needed path has
   landed (`dbc1e9b` resize propagation, `2b021c6` scaling). Tell us what the
   game does when it fails (a settings screen? a title option? a window resize?)
   to turn this into a concrete repro.
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
* **Stale review doc** `docs/migration/reviews/fs_engine.md` §1.10 still records
  the `exists`-reports-directories finding as open; the code and
  `crates/engine/tests/storage.rs:353-365` now agree with `TVPIsExistentStorage`.
  Annotate it as resolved.

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

Three members still raise (or ignore an argument) where the reference returns a
value. Same class as the landed C-batch: a member behaves like the reference or
raises because the reference raises.

* **C5** `Font.doUserSelect` raises (`crates/compat/src/natives/visual/font.rs`);
  the reference returns `0` (`LayerIntf.cpp:11931-11956`).
* **C6** `Font.rasterizer = n` raises for `n != 0`; the reference ignores an
  out-of-range value (`LayerBitmapImpl.cpp:94-101`).
* **C11** `Font.getList(flags)` ignores `flags`; the reference passes it to
  `GetFontList` (`LayerIntf.cpp:11977`).
* **C14 (coverage)** `scripts/native_parity.py:59` still checks only
  `reference/cpp/core/**`; the plugin surface is machine-checked by
  `scripts/plugin_parity.py`, but plugin *behaviour* rests on the audits alone.
  `Mouse`/`Key` are in neither parity map (see §7).

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
* **IME** exists on Linux (`Ime::Commit → onKeyPress, split per character`);
  **Android ignores text input entirely** (`crates/android/src/input.rs:45-47`),
  which breaks any title that asks the player to type a name.

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
* On-device validation of everything: startup, decode, render, MediaCodec,
  `memfd`, touch mapping, surface recreation.
* Surface recreation restarts the game (`docs/audits/android-jni-review.md` F5).
* No drift guard between the Kotlin option constants (`RuntimeSettings.kt`) and
  the Rust ones (`api::config`) — they agree today by hand.
* The drag-slop threshold is a fixed 16 px where the reference scales by DPI
  (documented divergence).
* `docs/runtime-config.md` still carries a stale snippet (F10 in the JNI audit).
* The launcher's whole UI is hardcoded English; `strings.xml` holds only
  `app_name`, and no `values-zh` exists (`docs/audits/android-ui-review.md` F5).

## 12. Deferred design work

* **L2 owns the frame** — `Engine::frame`/`Engine::run`/`GameFrame` designed, not
  built (`docs/migration/02 §3`, `07 Phase 9`). Until then `platform::Clock` is a
  wall clock, which is why headless frame counts reach real time only by accident.
* **TJS VM Rust port** — deferred (`08 §10.7`).
* **`streams.cpp`: thin C++ adapter over a Rust implementation** — deferred. It
  is the only port-authored C++ besides the ABI boundary (794 lines), and every
  memory-safety defect in the save path was in it.
* **L2 API snapshot + CI** — `cargo public-api` is decided but unwired. There is
  no CI gate: the only workflow builds the Android app on tags and does not run
  the Rust gate, so §1 is run locally.
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
  0 missing and **2 intentional extras** (`KAGParser.multiLineTagEnabled`,
  `paramMacros`; `docs/native_parity.md`).

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

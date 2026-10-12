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

## 2. In flight

* **Stub implementations** — `Window.postInputEvent`, `System.exeName`,
  `System.versionString`, `Font.doUserSelect` (return `0`, not a raise), and the
  port-only `DrawDevice.name` removal (`implement-stub-natives`); the plugin
  stub rows (`MenuItem.popup`, `D3DAdaptor`, `SeparateLayerAdaptor`,
  `CDDA.onFadeCompleted`) under oracle-first review (`implement-stub-plugins`).
  Source: `docs/audits/stub-inventory.md`.
* **Android engine state** — the entry path fails on the second launch in a warm
  process (already-installed logger treated as fatal), the terminal state write
  clobbers recorded failures, and five fatal exits write no state
  (`fix-android-engine-state`, from `docs/audits/android-jni-review.md`).
* **Vulkan init error** — a device-enumeration failure reports itself as
  "render backend is not supported", which misattributes a broken driver
  (`fix-vulkan-init-error`).
* **Focus domains** — the reference gives every parentless layer its own focus
  manager (`docs/audits/focus-invalidate-events.md` Q2); the per-root change is
  in progress.
* **Android storage audit** — `docs/audits/android-storage-review.md` pending.

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

## 5. Win32 dialogs and popups — deferred to a platform layer

Decision: dialogs/popups belong to a **platform-level compatibility layer**, so a
script gets a native-looking dialog on whichever OS runs it; they are not faked
in compat. Open surface:

* the real `WIN32Dialog` API (`Header`, `Items`, `Bitmap`, `Blob`,
  `SolidBrush`, `getOctetAddress`, `getStringAddress`, `getStringFromAddress`,
  `initCommonControlsEx`, the instance API) — only the class ctor and
  `messageBox` are registered (`crates/compat/src/natives/plugins/win32_dialog/mod.rs:256-280`);
* `MenuItem.popup` / `popupEx` (the menu surface behind `TVPShowPopMenu`);
* the monitor dictionary's `name` string (MONITORINFOEX `szDevice`) that the
  host monitor tuple cannot supply (`crates/compat/src/natives/plugins/window_ex/system.rs`).

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
* **L2 API snapshot + CI build step** — `cargo public-api` decided, unwired; CI
  runs only `cargo fmt --check` + the stateless guard.
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

## 14. Audit reports

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

# TODO — open work

**Branch:** `main` · **repo:** `$KRKR_REPO` · **game:**
`$KRKR_TEST_GAME` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

This file lists **only what is still open**, in priority order; a landed change
is deleted from it and history is read with `git log`. Decisions live in
`docs/migration/**`, the working rules in `AGENTS.md` and `scripts/README.md`,
behaviour found correct in §17, and topic-to-doc pointers in `AGENTS.md` §11.
Every item was verified against the tree.

## 1. Priorities

1. **L2 owns the frame** (D26/D27). `Engine::step`/`frame` own the clock, timer
   pump, projection and render; `GameFrame::before_render(now_ms, dt_ms)` is the
   single upward hook. The VM tick and audio pump still run inside it
   (`crates/app/src/game.rs` `VmHook::before_render`), as do the video and
   transition pumps. Remaining: move those pumps into L2, retire
   `SoundState.clock` for an L2-owned delta, add `Engine::run(clock, hooks)`,
   and privatize `TimerState::advance` (`engine/src/timer.rs`).
   `docs/migration/07` phase 9.
2. **Image decode scheduling.** A transition loads ~60 images serially on the VM
   thread (~32 ms apart, ≈1.9 s end to end); a 6.77 s frame is the same sum of
   synchronous decodes (`docs/audits/image-load-cost.md`). The decoded-image
   cache is consulted on every load but disabled by the reference default
   (`System.graphicCacheLimit = 0`), so the lever is scheduling: overlap
   independent decodes on a worker pool handing results back to the VM thread
   (the sound decode-ahead pattern), plus a per-frame budget.
3. **The in-game menu form** (§5) — the largest compatibility blocker.
4. **Android device verification** (§14) — one real title run with its log
   captured is the release condition (D36).
5. **Native plugin coverage** (§10) — 35 of 53 plugin names unemulated — and the
   **comments-gate debt** (§15): 4145 baseline entries, four unsafe-note files.
6. **L3 behavioural audit** (§11) — the emulation lives in L3, and the current
   motion/transition defects show that surface lock, arity/parity and the
   static gates cannot see semantic loss (timeline evaluation, transition
   cache ownership, silent draw no-ops, dropped events). Audit every L3 module
   against `reference/cpp/**`, add a behavioural test with a reference
   citation for each script-observable path, and turn every silent fallback
   into the reference behaviour or a typed/logged error (D11, D36). The
   current motion/transition wave is a first sample, not the audit.
7. **Deferred design work** (§16) stays deferred.

## 2. Focus model

A focus domain exists per parentless layer, the window is bound to the first,
and a cross-domain re-parent raises `TVPCannotMoveToUnderOtherPrimaryLayer`. Two
residuals:

* **Input is still flattened across domains.** Hit-testing walks
  `Scene::window_layer_order` for the whole window (`crates/compat/src/event.rs`
  `hit_layer` → `Scene::layer_at`) where the reference binds input to manager
  index 0 (`LayerTreeOwnerImpl.h:21`), so a click can reach a layer in a second
  root the reference would never deliver to. `Scene::window_input_root` exists
  for this; the dispatch does not use it.
* **`is_primary`/`primary_layer` is per window, not per manager.** Every
  parentless root is primary of its own manager (`LayerIntf.cpp:1217-1222`), so
  hiding or moving a *second* root should raise
  `TVPCannotSetPrimaryInvisible`/`TVPCannotMovePrimaryOrSiblingless`; the port
  raises only for the window's first layer.

## 3. Real-game reports

1. **SIGSEGV while saving — open.** Of the four causes ranked in
   `docs/audits/save-crash.md`, C2 remains: the serializer recurses without a
   depth bound (`crates/tjs/cpp/tjs2/tjsArray.cpp:1161`,
   `tjsDictionary.cpp:642`; the cycle guard is the only guard). Close it by
   reproducing once and reading the decisive backtrace, not by guessing.
2. **The game cannot change resolution.** Fit presents every game uniformly
   scaled and letterboxed, so the game need not change the framebuffer
   resolution and the declared script size stays what the script sees. Owed: the
   ground truth — what the title does when a resolution change is requested
   (`docs/migration/11`).
3. **A doubled save path is not root-caused.** One log shows `G//G/savedata/`,
   then a failed `…/savedata//savecheck` open. `System.exePath`, `personalPath`
   and `savedGamesPath` all return `TVPGetAppPath()` and only `dataPath` is
   `<app>/savedata/`, yet an `exePath + <property>` expression still doubles.
   Needs the title's `Config.tjs` or a TJS trace of `saveDataLocation`; no
   title-specific patch (D28).

## 4. Rendering and layers

* **R8b/R9/R10** `hasImage = true` must raise `TVPLayerCannotHaveImage` when the
  layer type cannot carry an image (`LayerIntf.cpp:2489-2493`): the three
  non-image types still collapse into `BlendMode::SourceOver`
  (`natives/visual/layer/blend.rs`) and `LayerState` has no `can_have_image`.
  The steady-state projection draws the main image, never reads `Layer.face`
  (`engine/src/projection.rs` `append_window`), and builds nodes with
  `..RenderNode::default()`, so a `dfMask`/`dfProvince` layer is faithful only
  during a transition and a transform set without a repaint is lost
  (`AffineLayer` bakes it into the main image).
* **Auto-path error text.** `Storages.addAutoPath` enforces the trailing
  delimiter (`storages.rs` `validate_auto_path`) but raises `"path must end with
  '/', '\\' or '>'; got …"`; the reference raises
  `TVPMissingPathDelimiterAtLast` = `"Use path delimiter '>' or '/'"`
  (`MsgIntfInc.h:32`) — script-observable.

## 5. Dialogs and popups

The reference draws its own popup: `MenuItem.popup` calls `TVPShowPopMenu` and
returns `1` (`MenuItemImpl.cpp:309-315`), pushing the engine-drawn form at
`InGameMenuForm.cpp:14-115` whose activation removes it and fires the item's
`onClick`. Build that form behind L2 menu state (`docs/audits/menu-popup.md`); a
native OS popup would be a deliberate divergence, and Linux has nothing to call
(the XDG portal has no menu interface).

* **The in-game system menu is not drawn at all.** `menu_item_popup`
  (`natives/menu_item.rs`) keeps the tree in `MenuState` and returns `1` without
  drawing, so a title that gates settings behind a popup leaves the player
  stuck. Rows from `MenuState` → hit-test → activation → remove + `onClick` is
  the fix.
* **The `WIN32Dialog` surface is incomplete.** `register`
  (`plugins/win32_dialog/mod.rs`) installs only the class constructor and
  `messageBox`; `Header`, `Items`, `Bitmap`, `Blob`, `SolidBrush`,
  `getOctetAddress`, `getStringAddress`, `getStringFromAddress`,
  `initCommonControlsEx` and the instance API remain.

## 6. Core natives

* **C6/C11** `Font.rasterizer = n` raises for `n != 0` (`font.rs`
  `font_rasterizer_set`) where the reference ignores an out-of-range value
  (`TVPSetFontRasterizer`, `LayerBitmapImpl.cpp:94-101`), and
  `Font.getList(flags)` accepts `flags` and ignores it (`font_get_list` takes
  `_argv`) where the reference passes it to `GetFontList`
  (`LayerIntf.cpp:11977`).
* **C14 (coverage)** `native_parity.py` checks `reference/cpp/core/**/*.cpp`
  only: plugin surface is machine-checked by `plugin_parity.py`, but plugin
  *behaviour* rests on the audits alone; `Mouse`/`Key` are in neither map (§7).

## 7. Input / audio / video

* **M3 (P1)** `onCloseQuery` is registered as a no-op
  (`natives/visual/window.rs`) and `PlatformEvent::CloseRequested` exits
  outright (`crates/app/src/runner.rs`), so the reference's `OnCloseQueryCalled`
  veto (`TVPWindow.cpp:790-880`) cannot happen.
* **M5 (P2)** `getVisBuffer` reads the decoded source, not the device ring
  (`natives/sound.rs`); a positive `sampleAhead` past the decoded end yields
  silence where the reference reads the ring (`ahead = 0` is exact).
* **M8 (P1)** `Layer.openMovie`/`startMovie` raise where the reference logs and
  returns void (`layerExMovie.cpp:160-171`), so a `try/catch` observes a
  different outcome (`natives/plugins/layer_ex/movie.rs`).
* **M9 (P2)** window-level `onMouseEnter`/`onMouseLeave`/`onDoubleClick` are
  synthesized although the reference only declares them
  (`WindowIntf.h:465-485`); `onMouseDown` does not first run `PrimaryMouseMove`
  and `onMouseUp` does not re-check the layer under the cursor
  (`LayerManager.cpp:374-420`); `Mouse`/`Key` are port-only; `PlayStatus` lacks
  `"ready"` (`VideoOvlIntf.cpp:73,84`).
* **M9b/M9c (P2)** `Window.useMouseKey` is stored and never acted on — the
  reference's `InternalKeyDown` turns `VK_RETURN`/`VK_SPACE`/`VK_ESCAPE`/pad
  keys into synthetic mouse events while it is on (`TVPWindow.cpp:648-707`).
  `Window.postInputEvent` dispatches to the first window and its focused layer
  instead of `this` window: right for single-window titles, wrong for
  multi-window ones (`docs/audits/stub-inventory.md` §Remaining).
* **Input imprecisions** needing a decision or a source: click/double-click
  timing (30 frames, double on the second **release**, where Windows fires on
  the press); `onTouchScaling`/`onTouchRotate` never synthesized; layer-native
  `action` forwarding only from the window path.
* **IME / on-screen keyboard is incomplete.** Linux splits `Ime::Commit` per
  character into `onKeyPress`, with no xkb/keymap or dead-key handling;
  `Window.imeMode`/`Layer.imeMode` are stored and never acted on. **Android
  drops text input** (`crates/android/src/input.rs` ignores
  `TextEvent`/`TextAction`) and never requests the soft keyboard, so a title
  that asks the player to type a name cannot.

## 8. File system / XP3

* **F3/F4 (P1)** the "current path" is the loose app directory rather than the
  project archive (`crates/engine/src/storage.rs` `find_recorded`;
  `SysInitImpl.cpp:160-165`), so a loose file wins over the project-archive
  entry; and `find` also searches **every** mounted archive after the auto paths
  where the reference searches only the current folder then the auto-path table
  (`StorageIntf.cpp:1060-1105`). On-demand auto-path opening is implemented;
  these orderings are not.
* **F6/F8–F14** `Storages.getFullPath`/`getPlacedPath` return a normalized
  relative name or `arc>entry`, not the full `file://domain/path`
  (`natives/storages.rs`). `getLocalName` on a miss returns the input (the
  reference's file-media path needs an oracle decision);
  `isExistentStorageNoSearchNoNormalize` is case-sensitive on the host; host
  write semantics un-investigated.

## 9. KAG parser / plugin residuals

* **PN2 (P1)** the `xp3_filter` buffer is a TJS `Array` carrying `ptr`/`count`
  because the C ABI cannot expose a borrowed byte slice as a native object, and
  the filter script resolves free names against the game's global scope (the
  reference keeps them in a sub-engine), so a `global.x = …` there is visible to
  the game (`natives/plugins/xp3_filter/mod.rs`).
* **PN3 (oracle)** the vendored `reference/cpp/plugins/json/jsonPlugin.cpp` is a
  demonstration (`evalJSON` returns its input; `evalJSONStorage` does not
  parse), so the port implements real JSON semantics through `serde_json` plus a
  byte-exact writer; that divergence needs a docs entry outside the module
  comment, and has none.
* **PN5/PN8 (P2)** `dirlist` does not apply the reference's regular-file and
  directory filter (`dirlist.cpp:42`) because the delegated `Storages.dirlist`
  exposes no file-type field; `windowEx`'s `ncht*` are read-only getters where
  the reference registers writable data members (`windowEx.cpp:935-960`); and
  `plugin_parity.py` cannot see post-registration globals (`global.X = …`), so
  `getDirList`, `drawFFTGraph`, `FILE_ATTRIBUTE_*` and `ShortCutInitial*` have
  no coverage.

## 10. Native plugins

35 of the 53 plugin names games ship have no emulation — the `**no**` rows of
`plugins/INVENTORY.md`, the DLLs being gitignored — so each takes the
null-plugin path (D14): every member accepted, every call void, one warning.
Per plugin: recover the surface from the DLL with the `plugins/analysis/`
pipeline, emulate in `crates/compat/src/natives/plugins/<name>/`, register the
DLL name in `BUILTIN_PLUGINS`, add behaviour tests citing the DLL by path, hash
and function against a committed fixture — the DLL never loads at test time
(D38). Order: `plugins/analysis/PRIORITY.md`. `PackinOne` is the one unemulated
plugin real games actually link, so a storage proxy there needs behaviour, not
surface; `krmovie`/`yuzuex` need L1/L2 backends.

## 11. L3 blockers

* **L3 behavioural audit.** L1/L2 are small, stateless abstractions that the
  static gates cover well; the compatibility emulation is L3, where a member
  can exist with the right name/arity and still lose the reference semantics.
  The current wave fixed the motion evaluation clock, child-clip time, curves,
  events, queuing, the render cache and the transition self image; the audit
  remains open for the remaining motion GPU/L1 resource path, the visual
  layer/window pixel paths, KAG, each native class and plugin port, and
  audio/video/focus event dispatch. Audit L3 by subsystem against the reference oracle:
  motion player (timeline/eval/child motion/events/draw), visual layer and
  window, KAG, each native class and plugin port, audio/video/focus event
  dispatch. For each module record the oracle path, the script-observable
  paths, and the behavioural test that fails on the divergence; add real-game
  traces or committed fixtures where a unit test cannot reach the state. A
  divergence that must remain is recorded as an approximation with its
  reference gap (D36), never as "unobservable". Exit when every L3 module has
  a named oracle, a behaviour test and no silent script-observable fallback;
  the surface lock and parity reports remain the contract but are not the
  evidence of behaviour. Do not rewrite L1/L2 for this; change an L1/L2
  signature only where the L3 behaviour cannot otherwise match.
* `Window.registerMessageReceiver` records the receiver but **nothing dispatches
  messages** (`natives/visual/window.rs`; the registry is read only by its unit
  test, and `WindowImpl.cpp:2150-2163` forwards received messages to the
  platform), and `windowEx` dynamic events (`onMinimize`/`onResizing`/…) are
  probed by name and never raised: there is no native window-message dispatcher
  on Linux (`natives/plugins/window_ex/window.rs`).
* **Hatch `Percent50` paints 25%** where the reference's checkerboard paints
  `(x+y) % 2 == 0`, i.e. 50% (`engine/src/raster.rs` `hatch_foreground` style 12
  vs `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:683-689`);
  every other style matches.

## 12. VM and bytecode boundary

* **`unsafe impl Send + Sync for Tjs2Engine` is unsound as a public API**
  (`crates/tjs/src/lib.rs`) — two engines race process-global VM state — and
  nothing requires the bounds. Remove them and let the compiler name any caller
  (`docs/vm-defects.md` GLO-1 / LIFE-2).
* **`compileStorage` does not round-trip a `+=` or `++` statement.** The
  writer's `TranslateCodeAddress` registers only the base opcode of each
  arithmetic family, so `VM_ADDPD`/`VM_ADDPI`/`VM_ADDP` and the
  `VM_INC`/`VM_DEC` variants export code-generator byte offsets that the loader
  multiplies again. Loading a game's `.tjsb` is unaffected, because the reader's
  translation is the one that runs there. Patching the vendored table would make
  the reconstruction match itself, not the reference (AGENTS §2). **Open
  question:** does the shipped engine share the snapshot's incomplete export
  table? Settle it with a `.tjsb` from a shipped writer containing a `+=`-family
  or `++`/`--` statement. `docs/audits/bytecode-scripts.md` §5,
  `docs/vm-defects.md` BLK-3.

## 13. GPU memory

Open: the config-cycle growth is explained; the fixed descriptor pool and the
exit-time heap corruption are not. Symptom (windowed Vulkan, real title, after a
while of play): `failed to upload N bitmap(s): out of GPU memory: failed to
allocate a texture descriptor set`, every frame.

1. Grow or size the Vulkan descriptor pool from the live texture count instead
   of the fixed cap (`create_descriptor_pool`,
   `crates/platform/render/src/vulkan/pipelines.rs`), or establish the
   reference's bound. It exhausts past 1024 live textures or when a texture is
   dropped without `destroy`: a hard cap, not a cache limit, and
   `System.graphicCacheLimit` bounds no layers.
2. Fix the exit-time `free(): chunks in smallbin corrupted` abort; it reproduces
   on a no-config title run, so it is independent of layer count and caching.
   Audit the compat-context/FFI release order at teardown.
3. Keep the overlay object counters (windows/layers/bitmaps/fonts/textures) as
   regression evidence; descriptor-set and `ObjectRegistry` counts are missing.

## 14. Android

The Android target is **never built or tested on the development host**: no NDK
is installed and none may be installed. Android-only code is reviewed here and
run on a machine that has one; anything not compiled is documented as unverified
(`AGENTS.md` §0.3).

* **On-device validation of everything (D36)**: startup, decode, render,
  MediaCodec (compile-checked, never run), `memfd`, touch mapping, surface
  recreation and **font resolution** (a requested face loads), captured in
  `<filesDir>/krkr.log`. No release without this run; the cpal audio backend has
  never played on a device.
* **PSB/PSD L1 crate on Android.** `forkunion` has a C++ core; the host build
  compiles, but the aarch64 Android build and run are unverified. If it fails,
  use `forkunion`'s `portable` feature and record the result.
* **Platform and launcher gaps.** Nothing handles audio focus, pause/resume or
  losing focus, and surface recreation restarts the game from scratch
  (`docs/audits/android-jni-review.md` F5). No drift guard exists between the
  Kotlin option constants (`RuntimeOptions` in `android/.../RuntimeSettings.kt`)
  and the Rust ones (`crates/platform/api/src/config.rs`). The drag-slop
  threshold is a fixed 16 px where the reference scales by DPI, and the launcher
  UI is hardcoded English: `strings.xml` holds only `app_name`, with no
  `values-zh` (`docs/audits/android-ui-review.md` F5).

## 15. Comments-gate debt

`house-style` (`scripts/krkr_gates/checks/house_style.py`) enforces the D31
rules (AGENTS §6): no external-engine citation or history narration outside
tests, and no `#[allow(clippy::…)]`/`#[expect(clippy::…)]` anywhere. Neither has
an allowlist exemption.

* **Citation and history baseline.** Pre-existing findings live in
  `scripts/allowlists/house_style_baseline.txt` — JSON Lines keyed to the
  normalized comment line, **4145 entries over 170 files** (4423 finding sites).
  The baseline is **debt**: burn it down in file batches, largest first
  (`compat/src/natives/sound.rs` 247, `engine/src/scene.rs` 153,
  `compat/.../player_members.rs` 150, `system.rs` 131), deleting each entry as
  its comment is rewritten.
* **`unsafe` blocks with no `// SAFETY:` note.**
  `scripts/allowlists/layering_allow.txt` now baselines four files; each still
  carries the debt: `compat/.../motion_player/{emote,helpers,player_members}.rs`
  and `natives/scripts.rs`. Write a real safety argument per block (AGENTS §5;
  `docs/audits/unsafe-inventory.md` §2.6).

## 16. Deferred design work

* **TJS VM in Rust** (`docs/migration/08` §10.7) **and `streams.cpp` as a thin
  C++ adapter over it** — both deferred. `streams.cpp` is the only port-authored
  C++ besides the ABI boundary, and every save-path memory-safety defect was in
  it.
* **A GPU text path and an L3 render-target surface.** `RenderContent::Glyphs`
  is rejected by both renderers, `supports_glyph_atlas` is false everywhere, and
  `create_render_target`/`read_render_target` are plumbed by neither `engine`
  nor `compat`, so `D3DAdaptor.captureCanvas` cannot copy a rendered texture
  into its layer. Text stays CPU-rasterized into the layer bitmap (D34);
  `docs/optimization.md`.
* **Zero-copy image presentation** — uploads use a host-visible staging buffer;
  the surface path needs `AMediaCodec_getOutputImage`, which `ndk` lacks.
* **C++ `-O2 -flto`** — 89.5% compile / 92.5% exec of `-O2` on the
  `scripts/perf/` harness, but the objects become LLVM bitcode and only the
  Android link validates it, so it needs a device A/B. `-O3`,
  `-fvisibility=hidden` and `mimalloc` were measured and rejected
  (`docs/performance.md` §7).

## 17. Verified — do not re-open

Checked and found *correct* (or correctly inert); changing any of these needs
new evidence.

* **R6** `Font.getTextWidth`/`getTextHeight` are not rotated; only `getEsc*`
  apply the angle. **R7** `System.stayOnTop` is not a reference member. **M4**
  MIDI/CDDA are compiled out and both class sets are inert arity-checked no-ops.
  **PN9** reference-stub members stay as they are. **F7** `exists` on a
  directory matches `TVPIsExistentStorage`; **F1** a scanned archive is skipped
  with a warning, an explicit `--archive` entry is a typed error
  (`09-game-loading.md:71-75`).
* **Presentation and pixel translation are in place**: one Fit-with-letterbox
  rule (D39) and the VM↔device mapping owned by L2 (D40/D41), no
  `ScalingMode`/`--scaling` left. Render-resolution decoupling stays deferred
  (`07`).
* **GPU decoding is not the lever**: MediaCodec is configured with no surface,
  TLG is entropy-coded with serial dependencies, the pixels must return to CPU
  memory for `getPixel` (D34), and the renderer has no compute pipeline. The
  lever is scheduling (§1.2).
* **Parity is 100% with empty allowlists** — 0 missing and 3 intentional extras
  (`KAGParser.multiLineTagEnabled`, `paramMacros`,
  `PhaseVocoder.__TvpPhaseVocoder`; `docs/native_parity.md`) — but 100% parity
  is not behaviour coverage (§6 C14, §9 PN8).
* **`VideoOverlay` frames** composite into `layer1`/`layer2`; start-up cost is
  measurable per script, native and phase (`docs/startup-attribution.md`).
* **The error-handling scan over-reports by design** (it cannot tell a `Result`
  receiver from an `Option` one); a site with no
  `scripts/allowlists/error_handling_allow.txt` entry fails the gate. Startup
  slowness on the game mount is environmental (FUSE), not code.

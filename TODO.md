# TODO — open work

**Branch:** `main` · **repo:** `$KRKR_REPO` · **game:**
`$KRKR_TEST_GAME` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

This file lists **only what is still open**, in priority order; a landed change
is deleted from it and history is read with `git log`. The current contract
lives in `docs/arch/**` and `docs/rules/**`, the working rules in `AGENTS.md`
and `scripts/README.md`, behaviour found correct in §17, and topic-to-doc
pointers in `AGENTS.md` §11. The superseded design record and the read-only
audits are archived in `docs.old/`. Every item was verified against the tree.

## 1. Priorities

1. **L2 owns the frame** (D26/D27). `Engine::step`/`frame` own the clock, timer
   pump, projection and render; `GameFrame::before_render(now_ms, dt_ms)` is the
   single upward hook. The VM tick and audio pump still run inside it
   (`crates/app/src/game.rs` `VmHook::before_render`), as do the video and
   transition pumps. Remaining: move those pumps into L2, retire
   `SoundState.clock` for an L2-owned delta, add `Engine::run(clock, hooks)`,
   and privatize `TimerState::advance` (`engine/src/timer.rs`).
   `docs.old/migration/07` phase 9.
2. **Deferred image decode pipeline — landed.** The L2 pending-image pool and
   the L3 load wiring are complete: loads probe synchronously, queue the
   decode, resolve at every pixel seam or the frame barrier, and enter the
   decoded-image cache under the load key. Residuals are in §18 (the
   synchronous province path and the Android verification); the
   pixel-translation follow-up is §19.
3. **The in-game menu form** (§5) — the largest compatibility blocker.
4. **Android device verification** (§14) — one real title run with its log
   captured is the release condition (D36).
5. **Native plugin coverage** (§10) — 35 of 53 plugin names unemulated — and the
   **comments-gate debt** (§15): 4126 baseline entries, four unsafe-note files.
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
   `docs.old/audits/save-crash.md`, C2 remains: the serializer recurses without a
   depth bound (`crates/tjs/cpp/tjs2/tjsArray.cpp:1161`,
   `tjsDictionary.cpp:642`; the cycle guard is the only guard). Close it by
   reproducing once and reading the decisive backtrace, not by guessing.
2. **The game cannot change resolution.** Fit presents every game uniformly
   scaled and letterboxed, so the game need not change the framebuffer
   resolution and the declared script size stays what the script sees. Owed: the
   ground truth — what the title does when a resolution change is requested
   (`docs.old/migration/11`).
3. **A doubled save path is not root-caused.** One log shows `G//G/savedata/`,
   then a failed `…/savedata//savecheck` open. `System.exePath`, `personalPath`
   and `savedGamesPath` all return `TVPGetAppPath()` and only `dataPath` is
   `<app>/savedata/`, yet an `exePath + <property>` expression still doubles.
   Needs the title's `Config.tjs` or a TJS trace of `saveDataLocation`; no
   title-specific patch (D28).
   * Traced in the test3 title: `System.dataPath` is `<app>/savedata/` and the
     title appends `"/savecheck"`, so the *doubled slash* is the title's own
     concatenation; the resolved host path is `<app>/savedata/savecheck`. The
     open failed because the data directory did not exist — the reference
     creates the parent folders when it opens a local file for write
     (`tTVPLocalFileStream`, `reference/cpp/core/base/impl/StorageImpl.cpp:618-641`),
     and the port did not, so the title's `checkSave` failed and every later
     save was refused. The port now creates those folders; the residual
     question is the `G//G/` spelling in the original log, which this title
     does not produce.

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
`onClick`. Build that form behind L2 menu state (`docs.old/audits/menu-popup.md`); a
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
  multi-window ones (`docs.old/audits/stub-inventory.md` §Remaining).
* **HE-AAC decode is unverified end to end.** The built-in decoder has no SBR
  synthesis, so an HE-AAC track is handed to the platform backend (FFmpeg on
  Linux, MediaCodec on Android). There is no committed HE-AAC fixture — the
  test game's HE-AAC voice track is not committed — so the SBR band is
  unverified here, and the Android path has never run on a device.
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
* **The M2 motion path is CPU-rasterized and is the title's frame budget.**
  `Motion.Player.draw` composes every icon into the target bitmap with the L2
  `affine_blit`; before the optimization the 千恋万花 title motion
  (`title_bg.mtn`) cost roughly **11–14 ms/frame of CPU in a release run**
  (~85 % `raster::blend_pixel_at` / `layer_ops::affine_nearest_band`). The
  reference submits cached textured quads to D3D
  (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:1100`,
  `PlayerRenderTargets.cpp:344-366`). Landed: node four-vertex alpha tint
  (`SourceCache.cpp:82-150`), no unconditional clear
  (`PlayerRenderTargets.cpp:1127-1131`), `imageModified` without a structure
  revision, opaque copy fast path, per-pass blend-kernel dispatch, integer
  nearest sampling (no `floorf`), a pixel-work thread threshold instead of a
  thread per small blit, and a real L2 dirty rectangle: `BitmapState.dirty_region`,
  `mark_dirty_rect`, partial texture uploads, and partial marks for
  `affine_blit`/`operate_rect`/`stretch_blit_mode`. The release 40 000-frame
  headless run dropped from **24.2 s to 17.3 s wall**. Remaining, in impact
  order: a GPU motion node type (L1 batched textured quads), a persistent
  worker pool (the threshold only removes the small-blit spawns), per-snapshot
  icon-path keys instead of per-frame `format!`, and reusable group/mask
  offscreens. Per-frame `snapshot.clone()` is an `Arc` clone, not the deep
  `PsbValue` copy the first audit suspected.
* **The in-game character art is a full-resolution WebP, and its decode is the
  avatar/expression switch's frame cost.** The 千恋万花 stand system
  (`system/StandImage.tjs` → `StandLayer.tjs` → `AffineSourceStand.tjs`, the
  `.tlg` files of `fgimage1080.xp3`) loads the character art through
  `Layer.loadImages`; the art is a **WebP** (a RIFF `VP8X` container) of
  **1445×3304** pixels (the half-resolution `_0_` variant is 725×1652).
  Measured in a release **windowed Vulkan** run of the game mirror (the
  archive mirror, the probe loading `fgimage1080/レナb_598.tlg` into a visible
  1920×1080 layer):

  | step | first load | repeat (cache hit) |
  |---|---|---|
  | storage read (the XP3 entry) | 2–38 ms | 1–2 ms |
  | header probe | 1 ms | 1 ms |
  | **WebP decode** | **90–120 ms** | 0 |
  | install, upload, render | 12–16 ms | 12–16 ms |

  The decode runs on the decode-pool worker, but the frame barrier
  (`Engine::advance` → `Scene::resolve_pending_images`) waits for it, so the
  frame that first shows a character runs at **7–9 fps** — the reported
  symptom. The same load twice is a decoded-image-cache hit (the game's
  `main/Config.tjs` sets `System.graphicCacheLimit = gcsAuto`, so the cache
  holds 512 MB), and two loads in one frame share one decode
  (`crates/compat/tests/pending_image.rs`
  `two_loads_of_the_same_image_in_one_frame_share_one_decode`): there is **no
  duplicate decode to remove**. The affine composite of the sprite costs
  1–2 ms and the full-texture upload 2–4 ms, so neither is the cost.
  **Landed:** the copy and blend passes (`blit_copy_clipped`, `blit_plane`,
  `blit_over`, `stretch_blit_plane`) mark the rectangle they wrote instead of
  the whole bitmap, so the sprite's texture is no longer re-uploaded whole
  when the game copies a face into it; the affine, `operate_rect` and
  `stretch_blit_mode` passes already marked their rectangle.
  **Remaining:** the decode itself. The reference decodes WebP with **libwebp**
  (`reference/cpp/core/visual/LoadWEBP.cpp:1-48`, `WebPDecode`); the port
  uses the pure-Rust `image-webp` (≈40 Mpixel/s against libwebp's ≈100
  Mpixel/s), and the workspace has no native WebP decoder. A native decoder
  is the only way to close that 2–3x; until then the first load of each
  character art costs a frame.
* **The title-load `AsyncTrigger` callback stalls the frame loop and skips the
  title motion.** A game `AsyncTrigger` (`id=5`, unnamed action) runs in
  `async_trigger_poll` for **1.8–5.7 s** on the title load (measured in a debug
  headless run; the release slow-frame line shows the same `async` phase). The
  title_bg `title` motion is created before the stall and the next
  `Motion.Player.progress` receives `dt=2417 ms`; at 60 frames/s that is
  `145.02` frames, past the clip's 120-frame `lastTime`, so the timeline stops
  and the 2 s title animation is never shown. The m2logo `back_white` player is
  unaffected (91 frames over ~1.5 s, `playing=["back_white"]`).
  **Measured root cause: the `PSBFile.root` octet path, not the motion.**
  The callback is the title scenario — `data/sysscn/title.ks` (`*init` →
  `*logo` → `*title` → `*title_common` → `[dialog name="title"]`) with the
  `patch/custom.ks` syshooks — and the title UI it builds comes from the PSB
  containers `window.pimg` / `title.pimg`. The port's `psb_root_get` →
  `set_octet_member` builds a **hex TJS script per octet member** and
  re-parses it (`global.__krkr_psb_buf[global.__krkr_psb_key] = <% xx xx … %>;`):
  **150 calls cost 5.349 s**, and the **158 `exec_script` calls** behind them
  cost **6.941 s**. The reference builds the octet as a *value* —
  `PSBResource::toTJSVal` returns `tTJSVariant(this->data.data(),
  this->data.size())` (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`) —
  so no script text exists to parse. The stall is pure CPU, not I/O: over the
  2.4 s gap the process reads only the 387 KB `window.pimg` entry
  (`/proc/<pid>/io` `rchar` is flat) and the TJS interpreter itself uses
  ~0.2 s of the whole run, so neither the storage nor the script is the cost.
  **Fixed:** the native `Tjs2Engine::set_member_octet` installs the member as
  a `VAL_OCTET` value, and `PSBFile` no longer generates or parses a script.
  The release title load fell from ~5.1 s to **46 ms** (22 000-frame run) and
  the title animation plays; no motion `dt` clamp was needed. `perf record` is
  blocked on this host by `kernel.perf_event_paranoid=2` (software `perf stat`
  works); gprofng works and is what the timings above come from.
* **The `Layer`/`layer_ex` affine paths now honour the destination clip.** The
  reference passes the clip into the blit —
  `MainImage->AffineBlt(ClipRect, …)` (`reference/cpp/core/visual/
  LayerIntf.cpp:4736`) — and writes inside that rectangle only
  (`reference/cpp/core/visual/LayerBitmapIntf.cpp:3960-4000`). The L2
  `affine_blit_clipped` writes the clip alone; `Layer.affine*`,
  `ImageFunction.operateAffine` and `layer_ex`'s `blit_with_transform` /
  `blit_parallelogram` now pass their clip and no longer clone/restore the
  whole bitmap. The layerExDraw plugin's `clipLeft/…` members remain
  unmodelled (pre-existing surface gap), so its affine path uses the layer's
  clip.
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
  (`docs.old/vm-defects.md` GLO-1 / LIFE-2).
* **`compileStorage` does not round-trip a `+=` or `++` statement.** The
  writer's `TranslateCodeAddress` registers only the base opcode of each
  arithmetic family, so `VM_ADDPD`/`VM_ADDPI`/`VM_ADDP` and the
  `VM_INC`/`VM_DEC` variants export code-generator byte offsets that the loader
  multiplies again. Loading a game's `.tjsb` is unaffected, because the reader's
  translation is the one that runs there. Patching the vendored table would make
  the reconstruction match itself, not the reference (AGENTS §2). **Open
  question:** does the shipped engine share the snapshot's incomplete export
  table? Settle it with a `.tjsb` from a shipped writer containing a `+=`-family
  or `++`/`--` statement. `docs.old/audits/bytecode-scripts.md` §5,
  `docs.old/vm-defects.md` BLK-3.

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
(`AGENTS.md` §0.4).

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
  (`docs.old/audits/android-jni-review.md` F5). No drift guard exists between the
  Kotlin option constants (`RuntimeOptions` in `android/.../RuntimeSettings.kt`)
  and the Rust ones (`crates/platform/api/src/config.rs`). The drag-slop
  threshold is a fixed 16 px where the reference scales by DPI, and the launcher
  UI is hardcoded English: `strings.xml` holds only `app_name`, with no
  `values-zh` (`docs.old/audits/android-ui-review.md` F5).

## 15. Comments-gate debt

`house-style` (`scripts/krkr_gates/checks/house_style.py`) enforces the D31
rules (AGENTS §6): no external-engine citation or history narration outside
tests, and no `#[allow(clippy::…)]`/`#[expect(clippy::…)]` anywhere. Neither has
an allowlist exemption.

* **Citation and history baseline.** Pre-existing findings live in
  `scripts/allowlists/house_style_baseline.txt` — JSON Lines keyed to the
  normalized comment line, **4126 entries over 170 files** (4403 finding sites).
  The baseline is **debt**: burn it down in file batches, largest first
  (`compat/src/natives/sound.rs` 247, `engine/src/scene.rs` 153,
  `compat/.../player_members.rs` 150, `system.rs` 131), deleting each entry as
  its comment is rewritten.
* **`unsafe` blocks with no `// SAFETY:` note.**
  `scripts/allowlists/layering_allow.txt` now baselines four files; each still
  carries the debt: `compat/.../motion_player/{emote,helpers,player_members}.rs`
  and `natives/scripts.rs`. Write a real safety argument per block (AGENTS §5;
  `docs.old/audits/unsafe-inventory.md` §2.6).

## 16. Deferred design work

* **TJS VM in Rust** (`docs.old/migration/08` §10.7) **and `streams.cpp` as a thin
  C++ adapter over it** — both deferred. `streams.cpp` is the only port-authored
  C++ besides the ABI boundary, and every save-path memory-safety defect was in
  it.
* **Generic L2 shader loading for all-shader transitions.** The public API is
  a generic shader/pipeline loader in `engine` (shader binary, pipeline
  descriptor, bindings, uniforms), not a transition-specific effect API. L3
  loads/selects shaders and supplies parameters through `engine_api`; L1 keeps
  only the backend mechanism and is never named by L3. The CPU transition path
  remains the null/headless implementation and the pixel oracle. This replaces
  the interim two-texture composition.
* **A GPU text path and an L3 render-target surface.** `RenderContent::Glyphs`
  is rejected by both renderers, `supports_glyph_atlas` is false everywhere, and
  `create_render_target`/`read_render_target` are plumbed by neither `engine`
  nor `compat`, so `D3DAdaptor.captureCanvas` cannot copy a rendered texture
  into its layer. Text stays CPU-rasterized into the layer bitmap (D34);
  `docs.old/optimization.md`.
* **Zero-copy image presentation** — uploads use a host-visible staging buffer;
  the surface path needs `AMediaCodec_getOutputImage`, which `ndk` lacks.
* **C++ `-O2 -flto`** — 89.5% compile / 92.5% exec of `-O2` on the
  `scripts/perf/` harness, but the objects become LLVM bitcode and only the
  Android link validates it, so it needs a device A/B. `-O3`,
  `-fvisibility=hidden` and `mimalloc` were measured and rejected
  (`docs.old/performance.md` §7).

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
  measurable per script, native and phase (`docs.old/startup-attribution.md`).
* **The error-handling scan over-reports by design** (it cannot tell a `Result`
  receiver from an `Option` one); a site with no
  `scripts/allowlists/error_handling_allow.txt` entry fails the gate. Startup
  slowness on the game mount is environmental (FUSE), not code.


## 18. Image-pipeline follow-ups

The deferred image pool, its L3 wiring, the cache insert and the startup
counters are landed. The whole workspace suite and the static gate are green.
The six earlier failures are closed:

* the motion mesh test now evaluates the previous-frame patch in normalized
  box coordinates instead of player-pixel coordinates;
* `dispatch_ui`, `kag`, `xp3_filter`, `xp3_filter_deferred` and `wuvorbis`
  link the lazy plugin that owns the member they use (`windowEx.dll`,
  `win32dialog.dll`, `KAGParserEx.dll`, `fstat.dll`, `wutcwf.dll`) before they
  create the object or open the stream — lazy registration is the reference
  behavior, not a test workaround;
* `Scene::resize_bitmap` resolves a pending image before preserving the
  overlap, so `Layer.setImageSize` and `Layer.openMovie` no longer panic;
* every `layer_ex` read/write seam resolves the pending image first;
* a deferred decode enters the decoded-image cache, and the startup log adds
  the pool's decode count and time to the synchronous codec counters.

Landed in this follow-up:

* Colour-keyed loads defer like unkeyed ones: the key runs as a worker
  pass, the keyed pixels enter the cache, and a repeated load is a hit.
* Province loads allocate first, tile a smaller image up to the main size, and
  deallocate on failure, matching `LayerIntf.cpp:2893-2914` and
  `GraphicsLoaderIntf.cpp:980-1031` / `:1092-1112`.
* `GdiPlus.Image` loads through the decoded-image cache, and
  `Layer.drawImage*` accepts an `Image` argument with its rotate/flip applied.

Remaining:

* Province loads stay synchronous: the plane's raw read seams (`getProvincePixel`,
  `provinceImageBuffer*`, layer hit-testing) take `&Scene`, so making them wait
  would require every read path to become mutable and would turn the deferral
  into a frame pause. A full-colour or colour-keyed load has one owning bitmap,
  which is why it can defer. The reference's province load is synchronous, so
  this is not a compatibility gap.
* The province loader still decodes RGBA and takes the red channel; the
  reference decodes `glmPalettized` and rejects true-colour sources, so a
  palettized source's plane index is not reproduced yet. The cache key also
  omits the desired main size (output is identical; only LRU/accounting).
  These are recorded residual gaps, not silent fallbacks.
* `Layer.drawImage*` also accepts the compatibility `Layer`/`Bitmap` shapes
  in addition to the reference's `GdiPlus.Image`; that widening created the
  pending-source path and is a compatibility surface change of its own.
* The Android pacing and `AHardwareBuffer` readback paths are uncompiled and
  unverified on a device (no NDK host); one NDK build remains required.
* The pool's counters are cumulative from pool creation; the app merges them
  before the startup snapshot, so they are a startup figure, not a live
  gauge.

## 19. Pixel-translation wiring decision

The L2 mapping is required and correctly placed: `Engine::vm_size()` reads the
primary-layer rect (L2 state), `surface_size()` comes from L1, and only L2's
projection/input seam can name both spaces. L3 must not name a device pixel;
`app` only applies the value.

The audit found that the two currently test-only methods have no correct
script-visible consumer:

* `raster_scale()` is one line, but using it requires the text path to stop
  compositing glyph coverage into the VM-resolution layer bitmap; the current
  softness at `s > 1` is a permitted D34 rasterization approximation.
* `vm_pixel_region()` is the device-access mapping for a **framebuffer**
  read. Every reference read (`Bitmap.getPixel`, `Layer.piledCopy`,
  `Layer.saveLayerImage`, province images, the layer_ex raw readers) addresses
  a bitmap, not the presented frame; routing those through the device backing
  would change script-observable values away from the reference.

Decision: keep the CPU VM bitmaps for every reference read, leave
`vm_pixel_region` unused rather than wire it into `getPixel`/`piledCopy`/
`saveLayerImage`, and treat a device-resolution capture as a separate additive
capability on the existing offscreen `RenderTarget` API (never as a change to
the reference members). `raster_scale` is deferred until the GPU text path is
production.

The per-node `dst`/clip bake is gone. The presentation is one transform on the
projected render-scene root: `PixelTranslation::root_transform()` returns
`Transform2D { scale: (s, s), translate: offset }`, which L1 already composes
during traversal. The render-only overlay carries the inverse transform so its
authored device pixels stay put. No L1 API change was needed, and `Affine2`
remains the backend's internal expansion of `Transform2D`; a full matrix field
would be over-general for a Fit-only mapping.

The remaining readback question is unchanged: `vm_pixel_region(x, y)` is the
device region a VM pixel covers, but no reference read addresses the presented
frame, so it stays unused rather than changing `getPixel`/`piledCopy`/
`saveLayerImage`. A device-resolution capture is a separate additive capability
on the offscreen `RenderTarget` API; `raster_scale` is deferred until the GPU
text path is production.

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

1. **L2 owns the frame** (D26/D27). `Engine::run(clock, hooks)`/
   `step`/`frame` own the clock, timer pump, projection and render;
   `GameFrame::before_render(now_ms, dt_ms)` is the single upward hook. The
   clock's source is injected once (`Engine::run`), the registry's pump is
   private to L2 (`engine::timer::deliver_due`), and the compatibility layer
   keeps no clock of its own — the sound poll and the housekeeping intervals
   read the engine's frame clock. Remaining: the video and transition pumps
   still run inside the hook, because the reference delivers the transitions
   and the video presentation as *continuous events*, inside the event
   delivery; their **time** is L2's (the transition's phase is the clock's
   tick difference, the picture's step is the clock's elapsed time). The
   mixer's advance is off-frame by design: the reference mixes on the
   device's callback. The TJS-visible `Timer` is a registry the frame pumps,
   where the reference computes its deadlines on a thread of its own.
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
  `hit_layer` → `Scene::layer_hit_next`) where the reference binds input to manager
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
* **The face selects the blend *method*, and the engine's blend table has
  no per-method kernels.** The layer keeps the reference's two faces: the
  outward face the script reads and writes (defaulting to `dfAuto` = `128`)
  and the actual draw face the operations use, resolved from the layer's
  display type (`LayerIntf.h:738-750`, `LayerIntf.cpp:371-372`,
  `:1525-1618`), so `Layer.face` returns the reference's number, a type
  change resolves the face again (`LayerIntf.cpp:1910`, `:1517-1518`) and
  the pixel operations follow the resolved face. The reference picks the
  blend *method* from the operation mode and the draw face
  (`LayerIntf.cpp:4217-4262`, `ImageFunction.cpp:78-130`) and gives each
  method its own arithmetic; the live dispatch is the render-method cache
  over the `tvpgl.cpp` kernels (`RenderManager.cpp:2396-2424`,
  `:2472-2565`, `:2854-2884`), and the per-method switch in
  `LayerBitmapIntf.cpp:1527-1826` sits inside `#if 0` — it states the same
  mapping but runs nowhere. The engine's table is keyed by the operation
  mode alone (`engine/src/layer_ops.rs`, `mode_kernel`), so three of the
  nine combinations still differ:
  * `omAlpha`/`dfAddAlpha` (`bmAlphaOnAddAlpha`, `LayerIntf.cpp:4224`)
    is the premultiplied over: the straight source is premultiplied
    (`TVPAlphaToAdditiveAlpha`, `tvpgl.h:169-174`) and
    `TVPAddAlphaBlend_a_a` (`tvpgl.h:116-137`) gives, per channel,
    `out = sat((dest * (255 - sa) >> 8) + src)` and
    `out_a = dopa - (dopa >> 8)` with `dopa = da + sa - (da * sa >> 8)`
    (`tvpgl.cpp:708-736`). The engine's source-over composites the
    straight source instead, so the destination's RGB differs by up to
    212/255. The other half, `omAlpha`/`dfAlpha` (`bmAlphaOnAlpha`), is
    the destination-alpha blend `TVPAlphaBlend_d` (`tvpgl.cpp:582-704`):
    with `addr = (sa << 8) | da`, `w = TVPOpacityOnOpacityTable[addr]` and
    `out_a = TVPNegativeMulTable[addr]`, per channel
    `out = dest + ((src - dest) * w >> 8)`, the two 64 KiB tables built in
    `f32` at `tvpgl.cpp:224-251`. The engine's straight source-over
    (`engine/src/raster.rs:70`) is that formula without the tables and
    differs by at most 6/255 on a channel (mean 0.5/255 over a 16×16 grid
    of source and destination alphas and five colours), and the reference's
    own OpenGL path (`ogl/RenderManager_ogl.cpp:3549-3557`) is the same
    formula in float, so this half is a rounding gap, not a structural one.
  * `omAddAlpha`/`dfAddAlpha` (`bmAddAlphaOnAddAlpha`, `:4239`) and
    `omAddAlpha`/`dfOpaque` (`bmAddAlpha`, `:4243`) are the
    additive-alpha blends: `TVPAddAlphaBlend_a_a` (`tvpgl.cpp:1058-1085`)
    for the first, and `TVPAddAlphaBlend_HDA_n_a` (`tvpgl.cpp:944-971`,
    `tvpgl.h:88-99`) for the second, which also holds the destination's
    alpha. The engine has no additive-alpha kernel and blends them
    source-over instead, which differs from the reference by up to 225/255
    on a channel. The third combination, `omAddAlpha`/`dfAlpha`
    (`bmAddAlphaOnAlpha`), is not implemented in the reference at all
    (`RenderManager.cpp:2557-2561`), and the operation now leaves the
    destination untouched here too.
  * `omOpaque`/`dfAlpha` (`bmCopyOnAlpha`, `:4250`) and
    `omOpaque`/`dfAddAlpha` (`bmCopyOnAddAlpha`, `:4254`) copy the
    source as an *opaque* image: `TVPCopyOpaqueImage`
    (`tvpgl.cpp:3326-3375`) copies the source's RGB and forces the
    destination's alpha to 255 (`src | 0xff000000`); below full opacity the
    reference blends instead (`TVPConstAlphaBlend_d`,
    `tvpgl.cpp:3506-3581`, and `TVPConstAlphaBlend_a`, `:3583-3613`).
    The engine's copy keeps the source's alpha, so the destination's alpha
    differs by up to 255/255, and only where the source carries a
    translucent pixel.
  The three formulas cost less per pixel than the source-over they would
  replace. Measured on the development host as the arithmetic alone (one
  thread, a 1920×1080 surface): the engine's straight source-over is
  11.0 ms/frame, the table-driven blend 5.1 ms/frame, the additive-alpha
  blend 6.4 ms/frame and the opaque copy 1.7 ms/frame — 0.2 ms/frame with
  AVX2, at the 0.18 ms/frame memory floor. An AVX2 gather over 32-bit
  tables is slower than the scalar byte-table lookup (6.8 against
  5.1 ms/frame) and the same formula without the tables is slower still
  (8.8 ms/frame), so the two lookups are a win, not a cost; the price of
  carrying the face is the dispatch, not the pixels.
  The face must not reach the engine as a face. What the engine needs is
  one generic fact — whether the destination's alpha is straight,
  premultiplied or absent — which is graphics vocabulary and belongs in the
  `api` crate next to `BlendMode`, re-exported through
  `engine_api.rs`, keying the engine's `mode_kernel` table as
  `(mode, dst_alpha)`. The kernel is then resolved once per pass instead
  of once per pixel (the current code re-resolves `mode_kernel(mode)` for
  every pixel, `engine/src/layer_ops.rs:2006`), and the same resolver can
  serve the affine path's own dispatch
  (`engine/src/layer_ops.rs:2133-2151`); a kernel function pointer in
  `BlitOptions` would have to be re-derived for the affine and stretch
  paths, and a new engine entry point per blend would duplicate the
  operate, stretch and affine loops.
  The differences are observable only where the additive-alpha type is in
  play: `omAlpha`/`dfAddAlpha` and both `omAddAlpha` combinations need a
  `dfAddAlpha` destination or an `ltAddAlpha` source, and `omOpaque`
  differs only in the destination's alpha and only for a translucent
  source. The common case, `omAlpha` onto the default `dfAlpha` face, is
  already within 6/255 of the reference.
  What is still open is the alpha-hold argument. The reference passes the
  layer's `holdAlpha` to every method (`LayerIntf.cpp:5250`) and the
  render-method cache is what honours it (`RenderManager.cpp:2396-2424`):
  only the copy methods have a second, hold-aware spelling
  (`CopyColor`), the plain alpha methods hold the destination's alpha
  unconditionally, and `bmCopyOnAddAlpha` has no hold spelling at all, so
  with the hold set and full opacity the operation does nothing
  (`RenderManager.cpp:2423`, `LayerBitmapIntf.cpp:1483-1485`). The compat
  derives the hold from the face instead
  (`natives/visual/layer/pixels.rs:365-380`), which is a different rule;
  whether to replace it is separate from the kernel table and still open.
  The `ImageFunction`'s mapping (`natives/image_function.rs`,
  `blit_method`) follows the face-derived rule too; the text members'
  `hda` argument and the layer's `holdAlpha` do not reach the text
  rasterizer (`ImageFunction.cpp:1024-1025`, `LayerIntf.cpp:4444-4468`),
  and the face's method selection for the text is the same arithmetic gap.
* **The transition's mix methods: what landed, and the alpha byte the
  reference's two paths disagree on.** The cross-fade's blend is chosen by
  the destination layer's type (`reference/cpp/core/visual/TransIntf.cpp:664-686`):
  a type that uses its alpha channel (`drawable.h:53-61`) gets the
  alpha-vs-alpha mix, the additive-alpha type (`drawable.h:63-65`) gets the
  four-channel mix, and every other type the plain one. The three mixes are
  `reference/cpp/core/visual/gl/blend_functor_c.h:662-672` (plain), `:673-680`
  (additive) and `:689-707` (alpha-vs-alpha), and the opacity table the last
  one reads is `reference/cpp/core/visual/tvpgl.cpp:224-251`. The reference's
  *software* path writes the plain mix's alpha byte as zero — the blend masks
  the green byte away and the alpha byte with it (`blend_functor_c.h:668-670`)
  — while its *graphics* path mixes the alpha with the phase
  (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`, whose script
  the plain and the additive type share, `:3274`); the reference's own two
  paths therefore disagree, and its shader comparison says so
  (`TEST_SHADER_IGNORE_ALPHA`, `:3270`). This port follows the graphics path
  on **both** of its paths — the CPU blend (`engine/src/trans/mod.rs`) and the
  cross-fade program (`engine/shaders/crossfade.frag`, which takes the mix
  from the uniform the transition's effect draw writes) — so the headless
  result and the desktop show the same transition. The residual is the
  software path's zeroed alpha byte: it is not observable in the drawn result
  (a destination that ignores its alpha is drawn opaque) and observable only
  in the alpha byte of the transition frame the CPU blend writes. The
  alpha-vs-alpha mix differs by at most one 8-bit level between the port's two
  paths, because the CPU path reads the byte-exact opacity table and the
  program computes the share in floating point — the same difference the
  reference's own two paths have.
* **The `wave` provider still mixes the four channels alike.** The
  reference's `wave` plugin picks its blend by the destination's type too,
  but it names the straight-alpha type alone (`reference/cpp/plugins/wave.cpp:238-246`,
  `LayerType == ltAlpha`), not the whole set of alpha-using types the core
  handlers name (`drawable.h:53-61`), so the generic handling the engine has
  does not single it out. The core cross-fade and universal handlers use the
  same three-way selection (`TransIntf.cpp:664-686` and `:737-761`) and are
  implemented; the `wave` provider keeps the four-channel mix.
* **The composited frame is the GPU path's remaining gap.** A host that
  cannot run the cross-fade program falls back to the composited frame, which
  draws the two sources with the phase as their weights and therefore mixes
  the four channels alike whatever the destination's type is
  (`engine/src/trans/mod.rs` `Trans::transition_frame`,
  `engine/src/projection.rs`). The rule-based kinds have no program at all and
  are blended on the CPU on every host, with the mix the destination's alpha
  handling selects.

* **The transition's mix methods: what landed, and the alpha byte the
  reference's two paths disagree on.** The cross-fade's blend is chosen by
  the destination layer's type (`reference/cpp/core/visual/TransIntf.cpp:664-686`):
  a type that uses its alpha channel (`drawable.h:53-61`) gets the
  alpha-vs-alpha mix, the additive-alpha type (`drawable.h:63-65`) gets the
  four-channel mix, and every other type the plain one. The three mixes are
  `reference/cpp/core/visual/gl/blend_functor_c.h:662-672` (plain), `:673-680`
  (additive) and `:689-707` (alpha-vs-alpha), and the opacity table the last
  one reads is `reference/cpp/core/visual/tvpgl.cpp:224-251`. The reference's
  *software* path writes the plain mix's alpha byte as zero — the blend masks
  the green byte away and the alpha byte with it (`blend_functor_c.h:668-670`)
  — while its *graphics* path mixes the alpha with the phase
  (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`, whose script
  the plain and the additive type share, `:3274`); the reference's own two
  paths therefore disagree, and its shader comparison says so
  (`TEST_SHADER_IGNORE_ALPHA`, `:3270`). This port follows the graphics path
  on **both** of its paths — the CPU blend (`engine/src/trans/mod.rs`) and the
  cross-fade program (`engine/shaders/crossfade.frag`, which takes the mix
  from the uniform the transition's effect draw writes) — so the headless
  result and the desktop show the same transition, and both match the build
  the user compares against. Two residuals remain against the software path.
  Its plain mix writes a zeroed alpha byte: that is not observable in the
  drawn result — a destination that ignores its alpha is drawn opaque — but
  it is observable in the alpha byte of the transition frame the CPU blend
  writes. Its alpha-vs-alpha mix takes the share from the byte table, whose
  rule for a transparent destination (`blend_functor_c.h:689-707` with the
  table at `tvpgl.cpp:224-251`) is 255 where the graphics path's
  `d.a * opacity / (… + 0.0001)` is zero, so a transparent source covers
  nothing here and covers everything there. The port's own two paths differ by
  at most one 8-bit level per channel, because the CPU path reads the share
  from a byte table and the program computes it in floating point — the same
  difference the reference's own two paths have.
* **The `wave` provider still mixes the four channels alike.** The
  reference's `wave` plugin picks its blend by the destination's type too,
  but it names the straight-alpha type alone (`reference/cpp/plugins/wave.cpp:238-246`,
  `LayerType == ltAlpha`), not the whole set of alpha-using types the core
  handlers name (`drawable.h:53-61`), so the generic handling the engine has
  does not single it out. The core cross-fade and universal handlers use the
  same three-way selection (`TransIntf.cpp:664-686` and `:737-761`) and are
  implemented; the `wave` provider keeps the four-channel mix.
* **The composited frame is the GPU path's remaining gap.** A host that
  cannot run the cross-fade program falls back to the composited frame, which
  draws the two sources with the phase as their weights — the reference's
  plain blend (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`)
  — so it mixes the four channels alike whatever the destination's type is
  (`engine/src/trans/mod.rs` `Trans::transition_frame`,
  `engine/src/projection.rs`). A destination that uses its alpha channel gets
  the coverage mix instead (`reference/cpp/core/visual/TransIntf.cpp:664-673`),
  which the composited frame cannot express: at a full-screen fade the two
  differ by up to five 8-bit levels per channel. The cross-fade program
  (`engine/shaders/crossfade.frag`) runs the coverage mix, so a host that can
  run it — the desktop and the Android build — matches the reference; the
  composited frame is the fallback for a host that cannot. The rule-based
  kinds have no program at all and are blended on the CPU on every host, with
  the mix the destination's alpha handling selects.
* **The shipped build's plugin surface is larger than the port's.** The
  shipped `textrender.dll` registers members the port does not have: `setFont`,
  `setStyle`, `timeScale`, `renderLines`, `contains`, `maxScrollOffset`,
  `maxScrollLine`, `calcLineOffset`, `getLinkNames`, `getLinkRects`,
  `getLinkCharacters`, `isLinkContains`, `getLinkOfPosition`,
  `defaultBigFontSize`, `defaultSmallFontSize`, `defaultAlign`, `defaultValign`,
  `defaultShadowColor`, `defaultShadowDiff` and `defaultEdgeColor` - adding
  them is a reviewed surface-lock baseline change.
* **The shipped build's `setOption` reads the option members** (`following`,
  `leading`, `begin`, `end`, `vertical`, `kinsoku_max`) where the reference
  tree's option deserializer does nothing
  (`reference/cpp/plugins/TextRender.cpp:137-158`), so the port's `setOption`
  still discards them and the game's message-layer path lays the text out
  with the compiled-in sets only.
* **The shipped build's default face is `normal`** (the reference tree's is
  `user`), and its colours are ARGB where the tree's are RGB.
* **The caption's centring is not the plugin's business.** The shipped build
  stores `align`, `valign` and `shadowdiff` and never reads them back, so the
  caption's offset comes from the game's own `FilterDrawTextParam`, which is
  defined in no archive of the shipped game.
* **The container layer types still collapse.** `ltBinder` (0), `ltEffect` (6)
  and `ltFilter` (7) read back as `ltAlpha` (2)
  (`natives/visual/layer/blend.rs`), so the resolution table's default arm
  (`dfOpaque`, `LayerIntf.cpp:1611-1613`) is unreachable and those layers
  resolve to `dfAlpha`; the reference gives them no image, so no face-sensitive
  operation can observe the difference there. A second parentless layer keeps
  the constructor's type and hit threshold here: the reference gives every
  parentless root its own manager and the opaque type and the zero threshold
  (`LayerIntf.cpp:463-472`), but its draw devices take one manager per window
  (`BasicDrawDevice.cpp:478-487`, `PassThroughDrawDevice.cpp:345-357`), so the
  second root raises there and its type and threshold are never observed; the
  per-window model is §2's residual.
* **What a fresh layer's image starts as is not established.** The reference's
  primary layer and a fresh `Bitmap` both start from the shared initial
  bitmap (`reference/cpp/core/visual/LayerIntf.cpp:214-220`,
  `reference/cpp/core/visual/BitmapIntf.cpp:41`), whose size and content the
  tree does not say: the holder that owns it is not vendored. Ours starts from
  an empty image that the size change fills with the layer's neutral colour -
  the reference's opaque white for the primary layer, the transparent white for
  a layer with a parent. Whether the reference's initial bitmap leaves any
  region of the grown image untouched is therefore an open question, not a
  known divergence.
* **The window's outer-rect members report the platform window's values,
  not the reference frontend's.** The reference's window form ignores
  `setPos`, `setMinSize` and `setMaxSize`, and its getters report
  `left`/`top` = 0, `minWidth`/`minHeight` = 0, `maxWidth`/`maxHeight` =
  1920/1080 and `borderStyle` = `bsNone`
  (`reference/cpp/core/environ/impl/TVPWindow.h:97-124`, the block the
  frontend marks "dummy function"; the native class only forwards,
  `reference/cpp/core/visual/WindowIntf.cpp:855-884`). Those are the
  Cocos2d frontend's values, not the engine's: the engine keeps whatever
  the form reports (`reference/cpp/core/visual/impl/WindowImpl.cpp:1559-1660`),
  and the engine-level rule in this family is the full-screen guard, which
  is implemented (`reference/cpp/core/visual/impl/WindowImpl.cpp:1183-1188`).
  Ours reports the values the platform window has, so a script that reads
  `win.maxWidth` sees what the platform says rather than the frontend's
  1920 — the divergence is scoped to the frontend's stub values.
* **A negative window size is clamped to zero.** The engine forwards the size
  it was given to the window form without validating it
  (`reference/cpp/core/visual/impl/WindowImpl.cpp:1583-1588`), and the
  engine models a layer's size as unsigned
  (`reference/cpp/core/visual/LayerIntf.cpp:2250-2252`), so neither raises
  nor clamps a negative window size. Ours clamps it to 0 because the L2
  window size is unsigned (`crates/engine/src/scene.rs`,
  `WindowState::inner_size`). A signed L2 size would carry the value the way
  the reference does; until then a negative size is a zero size here.
* **`Window.update(type)` cannot narrow the redraw.** The argument handling
  is the engine's own and is implemented: the type is read from the first
  argument, and only when a second argument is present
  (`reference/cpp/core/visual/WindowIntf.cpp:769-778`; the type is
  `utNormal`/`utEntire`, `reference/cpp/core/visual/WindowIntf.h:36-39`).
  What the type *does* is the frontend's: the reference's window form
  ignores it (`reference/cpp/core/environ/impl/TVPWindow.h:458-467`) and
  ours is drained without being read (`crates/app/src/game.rs`), so an
  update redraws the whole window in both.

* **The greyscale weighed the red and the blue the other way round.** The
  reference's grey is `(19*R + 183*G + 54*B) >> 8`: the packed pixel word's
  low byte carries the weight 19, the second byte 183 and the third byte 54,
  the sum is shifted right by eight and written to all three colour bytes
  while the alpha is held (`reference/cpp/core/visual/tvpgl.cpp:10350-10400`,
  the weights at `:10359-10365`; the shader spells the same ramp as 0.0743 /
  0.7148 / 0.2109, `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3970-3972`).
  The word holds the image's RGBA planes little-endian, so its low byte is
  the red plane (`reference/cpp/core/visual/LayerBitmapIntf.h:19-21` stores a
  colour in that byte order, `reference/cpp/core/visual/impl/LayerImpl.cpp:21-31`
  names the conversion, `reference/cpp/core/visual/RenderManager.cpp:346-350`
  reads the word back as the texture's RGBA planes). Ours weighed the red 54
  and the blue 19, so a pure red pixel came out at 53 instead of 18; the
  weights are now the reference's. The gamma ramp needed no change: the
  reference builds a table per channel and applies the red table to the
  word's low byte, the green table to the second byte and the blue table to
  the third (`reference/cpp/core/visual/tvpgl.cpp:10405-10460`,
  `:10473-10486`), which is what ours does.

* **The box blur's kernel and its alpha plane are still ours.** The blur
  averages every colour channel with the same weights, so the byte order
  cannot reach it. The reference has two implementations and they do not
  describe the same blur: the live one is a shader that sums nine samples
  and divides them by nine, with the samples at the corners and the edge
  midpoints of the blur area
  (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3991-4005`), while
  the other one - compiled out - averages the whole `(2x+1) x (2y+1)` window
  (`reference/cpp/core/visual/LayerBitmapIntf.cpp:4158-4406`, the `#if 0`
  at `:4090` takes it out of the build, and the call site is compiled out
  too, `:4424-4444`). Ours averages the whole window, which is the
  compiled-out one's rule. The alpha plane follows the kernel in both: the
  shader divides it by nine and the window's average divides it by the
  sample count (`reference/cpp/core/visual/argb.cpp:43-49`), while ours
  holds the alpha when the member is called with `isalpha` false
  (`natives/image_function.rs`). Closing this needs the reference's kernel
  to be settled first.

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

* **M3 (P2, deferred until the Rust TJS VM)** the window-close handshake is
  not wired: `onCloseQuery` is a no-op and `CloseRequested` exits before the
  script close hook runs, so the title Continue/suspend save is never written.
  The close/save/exit chain is deferred with the VM replacement.
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
* **The audio's playback cap is an explicit decision, not a side effect.** One
  frame may advance the mixer by at most `KRKR_RS_AUDIO_MAX_CHUNK_MS` (250 ms
  by default, `crates/app/src/audio.rs`); a longer frame leaves the playback
  that much behind the wall clock and reports it with a `warn`. The cap
  bounds the buffer one write produces, so a single write cannot jump the
  device's queue past its bound, and the picture is bounded by the same cap
  so the two fall behind together. The reference has no such cap: its mixer
  is fed by the device's own callback (`WaveMixer.cpp:342`), so its playback
  follows the device's real-time consumption and never lags — and its
  script-visible voice position follows that same consumption
  (`WaveImpl.cpp:3147`), not the tick count. The port keeps that shape (the
  rendered position on the frame clock, the device's latency subtracted), so
  the divergence is the *cap*: a frame longer than the cap plays behind.
* **The video advances on the frame clock, not on a media clock.** The
  reference's movie player runs on its own timeline, off the frame
  (`VideoPlayer.cpp:538`, `:1813`); only its *presentation* is on-frame,
  gated on the player's own PTS (`KRMovieLayer.cpp:45-67`). The port advances
  the overlay by the engine clock's elapsed time, bounded by the audio cap, so
  the picture runs at the frame rate and drifts against the media timeline
  whenever the two rates differ. That is an approximation with a known
  reference gap (D36): the position the script reads is the overlay's own
  position, and the movie's own voice is resynced to it, so the picture and
  the sound stay together — but neither follows the media clock the reference
  uses.

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
* **The shipped `textrender.dll` is not the reference's `TextRender.cpp`.**
  The game installs `textrender.dll` = wamsoft `textrend` 1.0.0.1 (2014),
  sha256[:12] `0a014cc2b581`; `reference/cpp/plugins/TextRender.cpp` is a
  different build of the same plugin name. They agree on the size, the
  spacing and the ruby defaults (24 / 6 / 10 / -2) and on the edge colour
  (`0x0080ff`), and they disagree on the style dictionary's member names:
  the shipped build reads `color`, `fontsize`, `linespacing`, `rubysize`,
  `rubyoffset`, `shadowcolor`, `edgecolor` and `linesize`, the reference
  reads `chColor`, `fontSize`, `lineSpacing`, `rubySize`, `rubyOffset`,
  `shadowColor`, `edgeColor` and `lineSize`. The game's own dictionary is in
  the shipped spelling (`sound.item` in the game's `default.tjs`: `color:
  0x442200`, `fontsize: 16`, `linespacing: 0`), so the plugin ignored it and
  the extra mode's music-list caption came out white at 24 px instead of the
  game's colour at 16 px. The port now reads both spellings, the lower-case
  one first (`crates/compat/src/natives/plugins/text_render/state.rs`,
  `style_member`; `crates/compat/tests/textrender.rs`).
  Four builds of the plugin exist and all four read the style dictionary in
  lower case only - `face`, `bold`, `fontsize`, `bigfontsize`, `smallfontsize`,
  `rubysize`, `rubyoffset`, `color`, `shadow`, `shadowcolor`, `shadowdiff`,
  `edge`, `edgecolor`, `linespacing`, `pitch`, `linesize`, `align`, `valign` -
  and no build reads a camelCase style member: those names are the class
  properties. The game reads the lower-case members out of its own style
  dictionary (`system/messagelayer.tjs`), and its own `smallsize` key is the
  game's vocabulary, not the plugin's - the builds read `smallfontsize`. The
  port reads what the shipped build reads, so the tree build's camelCase
  spelling and the shipped build's lower-case one both work.
  In the vendored tree `Plugins.link("textrender.dll")` registers the tree's
  own build - the loader resolves only statically compiled modules
  (`reference/cpp/core/plugin/PluginImpl.cpp:145`,
  `reference/cpp/core/plugin/ncbind.cpp:14-28`) and the tree's module name
  lower-cases to the same key (`reference/cpp/plugins/TextRender.cpp:5`) - so
  the game's shipped DLL never runs there, while a real engine loads the
  application's plugin directory and runs it. The tree and a real engine thus
  disagree about which build answers the link, and the shipped DLL is the
  oracle for this plugin's behaviour.
  Its `setOption` reads the option members (`following`, `leading`,
  `begin`, `end`, `vertical`, `kinsoku_max`) where the reference's option
  deserializer does nothing, so the port's `setOption` still discards them
  and the game's message-layer path (`setDefaultFromMessageLayer` passes the
  message layer's character sets) lays the text out with the compiled-in sets
  only. That is a behaviour change in the layout, and the reference tree says
  the option deserializer is a no-op, so it needs the integrator's decision.
  Still open, and a reviewed baseline change: the shipped build's class
  registers members the port does not have (`setFont`, `setStyle`,
  `timeScale`, `renderLines`, `contains`, `maxScrollOffset`, `maxScrollLine`,
  `calcLineOffset`, `getLinkNames`, `getLinkRects`, `getLinkCharacters`,
  `isLinkContains`, `getLinkOfPosition`, `defaultBigFontSize`,
  `defaultSmallFontSize`, `defaultAlign`, `defaultValign`,
  `defaultShadowColor`, `defaultShadowDiff`, `defaultEdgeColor`), and its
  style carries the big and the small font size and the shadow difference,
  which the port's style does not. Its `align`/`valign` are stored but never
  read by its layout, so the port's ignoring them is equivalent.
  The caption's *position* is therefore not the plugin's business: the extra
  mode's caption path (`txrend.draw`) puts the characters where the plugin's
  layout put them — the box's origin — and the offset comes from the game's
  own `FilterDrawTextParam("sound.txofs", …)`, whose function is in no
  archive of the shipped game (the patch's scripts call it, no script
  defines it), so the offset the shipped build uses cannot be read from the
  game either. The base class's caption path centres the text with
  `drawButtonCaptionWithAlign` (align 5, `x += (w - getTextWidth(text)) >> 1`,
  `y += (h - getTextHeight(text)) >> 1`) and the extra mode replaces that path
  with its own. Closing the centring needs the game's own offset dict and
  the text measurement, not the plugin.
* **The vendored tree's plugin loader never loads the `textrender.dll` the
  game ships; it registers the tree's own `TextRender.cpp`.**
  The tree's `Plugins.link` calls `TVPLoadPlugin`, which ends in
  `ncbAutoRegister::LoadModule` (`reference/cpp/core/plugin/PluginImpl.cpp:145`);
  `LoadModule` looks the lower-cased name up in the map of the statically
  compiled modules and loads nothing from the file system
  (`reference/cpp/core/plugin/ncbind.cpp:14-28`), and the tree's
  `TextRender.cpp` registers under `NCB_MODULE_NAME` = `TextRender.dll`
  (`reference/cpp/plugins/TextRender.cpp:5`), whose lower-cased key is
  `textrender.dll`. The `.tpm` autoload is not the path either: it scans the
  executable's folder, `system/` and `plugin/` for `*.tpm`
  (`reference/cpp/core/plugin/PluginImpl.cpp:74-88`), and the game ships
  `plugin/*.dll` and no `.tpm`. So in the tree the game's own DLL is never
  loaded and the tree's camel-case build answers every `Plugins.link`; a
  real KiriKiri2 build loads the application's plugin directory instead, so
  the game's own wamsoft `textrend` DLL is the one that runs there, it reads
  the lower-case names, and the caption comes out in the game's colour. The
  vendored tree and a real engine therefore disagree about which build of
  this plugin runs.
  The game links the plugin itself: `system/textrender.tjs` (in `data.xp3`)
  is compiled TJS2 bytecode whose top level is `global.TextRender = <the
  script's own class>` and then `Plugins.link("textrender.dll")` — the
  top-level object disassembles to `const %1, *3`, `spds %-1.*'TextRender',
  %1`, `cl %1`, `const %1, *0` (`textrender.dll`), `gpd %2, %-2.*'Plugins'`,
  `calld %0, %2.*'link'(%1)`, `srv %0`, `ret`. Nothing checks the result: the
  tree's `link` returns void and reports nothing
  (`reference/cpp/core/plugin/PluginImpl.cpp:299-311`), and a failed link
  surfaces later as a missing `TextRenderBase`.
  The shipped DLL is the oracle for this plugin's behaviour (the rule for an
  unported plugin): the port honours the shipped build's vocabulary — both
  spellings, the lower-case one first — and cites the DLL build by hash and
  gates on the committed fixture
  (`crates/compat/tests/fixtures/textrender_dll_state.txt`,
  `crates/compat/tests/textrender.rs`).
* **The plugin ships in four distinct builds, and every one of them reads
  the style dictionary in the lower-case spelling only.**
  The inventory holds six copies of `textrender.dll` in four builds
  (`plugins/INVENTORY.md`): `da1c09f846aa` (233472 bytes — the copies in
  `plugins/1`, `plugins/3` and `plugins/7`), `0a014cc2b581` (220672 —
  `plugins/2`), `44d7b0566c2f` (233472 — `plugins/5`) and `f180790ea6d1`
  (199680 — `plugins/6`). All four carry the same version resource (wamsoft,
  `textrend`, 1.0.0.1, copyright (C) 2014) and are four different binaries.
  Two of the games on this host ship the plugin: the directory
  `Others/doinaka_brother` ships `da1c09f846aa` and the game under test
  ships `0a014cc2b581`; a scan of the 486 DLLs under `/mnt/DATA/Games` for
  the `textrend` version string and for the file name found no other copy and
  no renamed one.

  All four read the same style members, and all of them in the lower-case
  spelling: `face`, `bold`, `fontsize`, `bigfontsize`, `smallfontsize`,
  `rubysize`, `rubyoffset`, `color`, `shadow`, `shadowcolor`, `shadowdiff`,
  `edge`, `edgecolor`, `linespacing`, `pitch`, `linesize`, `align` and
  `valign`; the two 233472-byte builds read `wordbreak` as well. The
  camel-case names in their string tables — `chColor`, `fontSize`,
  `lineSpacing`, `rubySize`, `rubyOffset`, `shadowColor`, `edgeColor`,
  `lineSize` — are the class property names, and `shadowColor`, `edgeColor`
  and `shadowDiff` also name the character dictionary's members; none of
  them is a style dictionary member in any shipped build. The shipped
  build's style deserializer is the function at `0x100022a0` of the
  `0a014cc2b581` build: it pushes each member name as a wide string and
  reads the dictionary through one of three member readers, and no
  camel-case name is among them.

  The game writes that spelling itself. The configuration entry
  `"sound.item"` in the game's `default.tjs` (in `patch.xp3`) is
  `%[ face:SystemDefaultFontFace, edge:false, shadow:false,
  fontsize:SizeCoef(16), smallsize:SizeCoef(12), linespacing:0,
  color:0x442200, valign:0 ]` and carries the comment `// for TextRender`,
  and the game's `system/messagelayer.tjs` builds the same dictionary with
  `fontsize`, `linespacing`, `rubysize`, `rubyoffset`, `shadowcolor` and
  `edgecolor` while reading the plugin's camel-case class properties
  (`defaultFontSize`, `defaultChColor`, `defaultRubySize`,
  `defaultRubyOffset`, `defaultShadow`, `defaultShadowColor`, `defaultEdge`,
  `defaultEdgeColor`, `defaultLineSpacing`, `defaultPitch`, `defaultAlign`,
  `defaultLineSize`, `defaultFace`, `defaultBold`) back. The game's
  `system/textrender.tjs` reads the same camel-case properties and the
  character dictionary's `color`, `edgeColor` and `shadowColor`, and calls
  the shipped build's `setDefaultFromMessageLayer`, `setDefaultFont` and
  `setDefaultStyle`, none of which the tree's build has. The game was
  written against the shipped build, and the lower-case spelling is the one
  its dictionaries carry. It reads that spelling back too: the game's own
  `system/messagelayer.tjs` reads `rubysize`, `rubyoffset`, `shadowcolor`
  and `edgecolor` out of its style dictionary in `setFont`, `rubysize`,
  `rubyoffset`, `shadowcolor`, `edgecolor` and `smallsize` in
  `setDefaultFont`, `linespacing` and `linesize` in `setStyle` and
  `setDefaultStyle`, and `fontsize` in `addButton`, while the same methods
  read the plugin's camel-case `default*` properties. The lower-case names
  are the game's own, and they are the names the shipped build reads.

  The port's two-spelling read therefore covers every build a real game
  ships: the lower-case spelling is the only one the shipped builds use and
  the port reads it first, while the camel-case spelling is the reference
  build's (`reference/cpp/plugins/TextRender.cpp:137-158`) and no shipped
  build is that build. The oracle for the lower-case names is the shipped
  DLL by hash — the derived fixture for the `0a014cc2b581` build is
  `crates/compat/tests/fixtures/textrender_dll_state.txt` and the citation
  is in `crates/compat/tests/textrender.rs`.

  Two gaps are left in the port's `deserialize`
  (`crates/compat/src/natives/plugins/text_render/state.rs`), and both are
  gaps against the shipped builds:
  * the port reads `italic` and `fontScale` from the dictionary and no
    shipped build does: the shipped build reads `bold` but not `italic`,
    and `fontScale` is a class property there. A dictionary carrying
    `italic` therefore renders italic in the port and upright in the shipped
    build; a dictionary carrying `fontScale` moves the port's `fontScale`
    property and nothing else.
  * the port reads neither `bigfontsize`, `smallfontsize` nor `shadowdiff`,
    which every shipped build reads, and the port's style has no field for
    the big and the small font size or for the shadow difference. A game
    that sets them gets the normal size and no shadow offset in the port.
    The shipped build's `align` and `valign` are stored but never read by
    its layout, so the port's ignoring them is equivalent.

* **`Layer.copyBlueToAlpha` is absent from the port and from the reference
  alike, and that is not a divergence.** The game post-processes every UI
  sprite's mask layer with it (`system/psdlayer.tjs`, ctx 17 `getDispLayer`:
  `lay.copyRect(left, top, maskLayer, 0,0,w,h); lay.copyBlueToAlpha();`), but
  the member exists neither in the port nor in the reference tree, so the
  game must guard the call and both engines skip it. The port therefore
  matches the reference here. It is recorded only because the member is part
  of the plugin family the game ships (the psd/psbfile DLLs): if that family
  is ever ported and brings the member with it, the surface lock changes
  (D29) and the sprite mask handling has to be re-derived.

## 11. L3 blockers

* **`omAddAlpha` on the alpha face is skipped instead of blended.** The
  compatibility layer treats the pair as unimplemented and leaves the
  destination untouched (`crates/compat/src/natives/visual/layer/pixels.rs`,
  `blend_is_unimplemented`), while the reference selects
  `bmAddAlphaOnAlpha` -> "AdditiveAlphaBlend_a"
  (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`,
  `reference/cpp/core/visual/RenderManager.cpp:2414-2424`). The engine now
  carries the additive-alpha kernel, so the pair only needs the reference's
  method selected instead of the no-op.


* **L3 behavioural audit.** L1/L2 are small, stateless abstractions that the
  static gates cover well; the compatibility emulation is L3, where a member
  can exist with the right name/arity and still lose the reference semantics.
  The current wave fixed the motion evaluation clock, child-clip time, curves,
  events, queuing, the render cache and the transition self image; the audit
  remains open for the remaining motion GPU/L1 resource path, the visual
  layer/window pixel paths, KAG, each native class and plugin port, and
  audio/video/focus event dispatch. The motion GPU path is the sharpest
  case: the reference renders a motion into a texture and copies it into the
  target layer, while the port's `Motion.D3DAdaptor.captureCanvas` copies
  nothing and `Motion.Player.draw` with such a target draws nothing at all,
  silently. Closing it needs a layer-tree primitive for a batch of textured
  quads, an L1 draw for that batch and the render-target plumbing in
  `engine` — the geometry is per-node affine, so neither the effect draw
  (one axis-aligned rectangle per pass) nor a layer node covers it. Audit L3
  by subsystem against the reference oracle:
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
  **The decode pool's byte limit refused the fourth part.** One part charges
  the pool about **19.6 MB** (the compressed bytes plus the **19 112 720**
  bytes of RGBA pixels it decodes into), and the pool's default limit was
  **64 MiB**: three parts fit (58.8 MB) and the **fourth** was refused, so the
  load fell back to a **synchronous decode on the frame thread** instead of
  decoding concurrently on a worker. The measured log line was `image: the
  decode pool refused "…" (the image decode pool would exceed its memory
  limit of 67108864 bytes); the load decodes synchronously`. The avatar
  switch loads several character parts, so it hit the refusal.
  **Landed:** the decode pool's default byte limit is now **256 MiB**
  (`engine::imagedecode::DEFAULT_MEMORY_LIMIT`), which holds the four
  character parts of an avatar switch at once (78.4 MB of charge) with room
  to spare; the refusal and its synchronous fallback are unchanged, and the
  refusal log now names the limit it refused the job against.
  **Remaining:** the decode itself. The reference decodes WebP with **libwebp**
  (`reference/cpp/core/visual/LoadWEBP.cpp:1-48`, `WebPDecode`); the port
  uses the pure-Rust `image-webp` (≈40 Mpixel/s against libwebp's ≈100
  Mpixel/s), and the workspace has no native WebP decoder. Measured on the
  1445×3304 art, one decode costs **~105 ms** with `image-webp` against
  **~46 ms** with libwebp. A native decoder is the only way to close that
  2–3x; until then the first load of each character art costs a frame.
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
* **The title's background animation is fixed; what the reference shows
  after the two-second intro is not established.**
  * **The motion player did not give the target layer the size of the canvas
    it draws into.** The environment layer the title's motion paints into
    (`title_bg`) has an empty rectangle, and a layer with an empty rectangle
    draws nothing at all, so the pose the player computed into the layer's
    image never reached the screen. The layer's rectangle and image are the
    game's own script-set members, and the reference reports the same
    script-visible values, so the layer is what it is and the player owes it
    the canvas size. The reference asks the target layer for the size of the
    canvas it is drawn into — the layer's own rectangle, and its image when
    that rectangle is empty
    (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:346-361`) —
    and gives the layer that size before the pose is drawn
    (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1126-1131`).
    **Fixed:** the motion player now hands the target layer the size of the
    canvas it draws into before the pose is drawn, and clips the pose to that
    canvas (`crates/compat/src/natives/plugins/motion_player/player.rs`); the
    behaviour test (`crates/compat/tests/env_image.rs`) pins the canvas size
    the target layer keeps.
  * **The engine had two clocks and the game read the wrong one.**
    `System.getTickCount()` returned a wall clock while the engine owns the
    frame clock, and the game's Conductor schedules its next timer tick from
    `getTickCount` — so in a headless run every KAG timer and `[wait]` starved
    and the scenario hung in silence at the caution screen's voice wait. The
    tick count is the engine's frame clock now, so the game and its timers
    share one clock (the reference has one clock too,
    `reference/cpp/core/base/impl/SystemImpl.cpp:622-629`). With that the
    scenario reaches the title, and the title's background motion is computed:
    every frame of the motion changes the layer's pixels, and only the frame
    after the clip's end repeats — the asset's own one-shot behaviour, which
    the reference shares (`title_bg.mtn`'s `title` clip carries `loopTime -1`).
  * **What is displayed AFTER the two-second intro may still differ.** The
    game's `custom.ks:*title_start` has two branches — the last `.mtn` frame
    and the `title_bg.png` stage image — and which one the reference shows is
    not yet established.
  * **The world's per-frame env update: there is none, and ours is not
    short.** An earlier reading took `world.updateAll`, `world.updateSource`
    and `world.refresh` never being called for a missing per-frame world
    pump, and took `world.envlayerList` holding only the reached objects for
    a lost registration. Both readings were wrong, and the measurement that
    settles them is the one taken AT THE TITLE: there
    `kag.env.objects` and `world.envobjects` hold the same pair
    (`bgm`, `title_bg`) and `kag.env.layerList` and
    `world.envlayerList` hold the same one (`title_bg`) — the two
    registries are in sync and complete. The earlier counts came from a run
    that had not reached the title yet, where both registries held exactly
    `ev`. And the reference has no per-frame world update either:
    `KAGWorldPlugin.updateAll` is reached only from `updateAllTrans` and the
    `onRefresh`/`onSync`/`onResetAll` handlers, `KAGWorldPlugin.refresh`
    only from the save/restore path, and `EnvObjectWorld.updateSource` only
    from the tag-driven `_updateAll`/`updateDiff`/`syncObject`/`doAfter`/
    `prepareTransition` — all tag-driven. The reference's per-frame work for
    an environment layer is the layer DRAW path
    (`reference/cpp/core/visual/LayerIntf.cpp:6394-6407`, `onPaint` on every
    draw completion), which ours delivers. The flip chain that drives the
    title's motion is that draw path (`AffineLayer.entryFlip` ->
    `window.entryFlipLayer` -> `flipLayers` -> `onFlipTimerInterval` ->
    `updateFlipLayers(dt)` -> `layer.updateFlip(dt)`, and `updateFlip` does
    `_interval += delta`). No engine-side divergence here.
    * Still open, and cheap to settle: the reference-side confirmation of the
      tag-driven reading (a run that opens the mini player or the stand view —
      the only scripts that reference `updateAll`), and what the reference
      shows AFTER the title's two-second intro (the last `.mtn` frame or the
      `title_bg.png` stage image; both engines should freeze, the clip being
      a one-shot).
  * **A probe lesson worth keeping:** adding a member to a script class at
    runtime perturbs that class's property dispatch, so a probe that mutates a
    game class can manufacture an error the engine never had. Wrap the
    INSTANCE, or hook `Scripts.execStorage` and wrap the class the moment its
    script finishes loading. The jump targets in the compiled scripts are
    RELATIVE, and `VM_JF` jumps if the flag is true while `VM_JNF` jumps if it
    is false (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:898-965`). Two
    more TJS2 traps for probes: `%` is the dictionary-literal token and not a
    modulo operator, and an empty `catch(e) {}` block is a syntax error.
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

Open: the config-cycle growth is explained; the fixed descriptor pool is not.
Symptom (windowed Vulkan, real title, after a while of play): `failed to upload
N bitmap(s): out of GPU memory: failed to allocate a texture descriptor set`,
every frame.

1. Grow or size the Vulkan descriptor pool from the live texture count instead
   of the fixed cap (`create_descriptor_pool`,
   `crates/platform/render/src/vulkan/pipelines.rs`), or establish the
   reference's bound. It exhausts past 1024 live textures or when a texture is
   dropped without `destroy`: a hard cap, not a cache limit, and
   `System.graphicCacheLimit` bounds no layers.
2. Keep the overlay object counters (windows/layers/bitmaps/fonts/textures) as
   regression evidence; descriptor-set and `ObjectRegistry` counts are missing.

## 14. Android

The Android target is **never built or tested on the development host**: no NDK
is installed and none may be installed. Android-only code is reviewed here and
run on a machine that has one; anything not compiled is documented as unverified
(`AGENTS.md` §0.4).

* **No Android toolchain on the host.** Downloading or installing the Android
  SDK, the NDK, the command-line tools or any platform/build-tools package is
  not permitted. The Android build belongs to the `apk` CI job and to a
  machine that has the toolchain. Kotlin changes in `android/` are reviewed by
  reading, and the launcher UI is therefore unverified until CI or a device
  builds it.

* **On-device validation of everything (D36)**: startup, decode, render,
  MediaCodec (compile-checked, never run), `memfd`, touch mapping, surface
  recreation and **font resolution** (a requested face loads), captured in
  `<filesDir>/krkr.log`. No release without this run; the cpal audio backend has
  never played on a device.
* **PSB/PSD L1 crate on Android.** `forkunion` has a C++ core; the host build
  compiles, but the aarch64 Android build and run are unverified. If it fails,
  use `forkunion`'s `portable` feature and record the result.
* **A device launch can die with no report in the engine log.** On a
  device, four launches of one title ended with the app process gone a few
  seconds after the game Activity started: the launcher's log shows the
  launcher Activity being recreated, the engine's log stops mid-startup, and
  the state file is left at `starting` with a dead pid. There is no Rust
  error, no panic message and no `failed` state, so the process died outside
  the engine's error paths: a native abort or a system kill. The engine log
  cannot say which, because a native abort leaves no trace in it. The
  launcher now reads Android's record of why the process ended
  (`ApplicationExitInfo`) and names the reason, and the engine writes a
  failure when a panic unwinds out of the entry point, so the next occurrence
  is diagnosable. **Open: the underlying death is not root-caused.** The
  decisive artefact is the device's own crash record (`adb logcat -b crash`,
  or `dumpsys dropbox data_app_native_crash`), which names the faulting
  library; the engine's log cannot.
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
  normalized comment line, **4076 entries over 168 files** (4352 finding sites).
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

* **The vendored reference tree and the classic engine disagree about the
  script-visible colour. The tree is the oracle; the classic is a recorded
  difference.** The tree in `reference/` is the **KrKr2 Emulator**
  (`github.com/2468785842/krkr2`), the cross-platform emulator with the
  Android build that this port is measured against; the classic engine
  (`krkrz/krkrz` @ `fd5c4baa6a2ef5978db1bd043634351f48667daf`) is the
  original KiriKiri2 the games were made for, and it differs from the tree
  in the colour domain:
  * the tree's internal pixel memory is RGBA bytes and every write path
    applies `TVP_REVRGB`
    (`reference/cpp/core/visual/LayerBitmapIntf.cpp:205` `SetPointMain`,
    `:236` `Fill`, `:385` `FillColor`), so a plain script colour is **drawn
    as named**; the classic's memory word is `0xAARRGGBB`
    (`visual/LayerBitmapIntf.cpp:218-223`) and it draws the same way.
  * the tree's *readback* returns the raw GL bytes
    (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:1079-1087`), so its
    `getPixel` reports `0x00BBGGRR`; the classic masks the alpha off its
    `0xAARRGGBB` word (`visual/BitmapIntf.cpp:49-54`) and reports
    `0x00RRGGBB`. The port follows the tree.
  * the tree's `drawGlyph`
    (`reference/cpp/core/visual/LayerIntf.cpp:4513`) and its image
    function's text and glyph paths
    (`reference/cpp/core/visual/ImageFunction.cpp:1058`, `:1160`) do **not**
    apply `TVP_REVRGB`, so they draw the red and the blue exchanged; the
    classic has no `TVP_REVRGB` at all and draws every path as named. The
    port follows the tree.
  * the tree resolves a system colour through a table keyed by the whole
    word, with the fallback that keeps the low 24 bits
    (`reference/cpp/core/environ/Application.cpp:874-940`); the classic
    resolves it through the OS, indexed by the low byte, and an index the OS
    does not know paints black (`visual/win32/TVPColor.h:37-42`). The port
    follows the tree.
  * the tree's grey ramp weighs the channels by byte position on its word
    (`reference/cpp/core/visual/tvpgl.cpp:10350-10390`); the classic's
    weighs them by byte position on the other word, so the red and the blue
    swap places (`visual/gl/blend_functor_c.h:883-891`). The port follows
    the tree.
  A game made for the classic may observe the classic's behaviour where the
  two differ; the port implements the tree, and each difference is recorded
  here so the choice is visible rather than accidental.
* **TJS VM in Rust** (`docs.old/migration/08` §10.7) **and `streams.cpp` as a thin
  C++ adapter over it** — both deferred. `streams.cpp` is the only port-authored
  C++ besides the ABI boundary, and every save-path memory-safety defect was in
  it.
* **The all-shader transitions need their own programs.** The generic loader
  landed with the effect draw path: `engine::shader` registers a program,
  creates and caches its pipeline, and the kinds that have a program
  (`crossfade`, `scroll`) are drawn by the projection instead of being
  blended into a bitmap. What is left is a program per kind that needs a
  per-pixel rule or a coordinate warp (`universal`, `mask` and the seven
  `extrans` kinds), and the selection of a program from `engine_api` (the
  seam carries the draw today, not a program). L1 keeps only the backend
  mechanism and is never named by L3. The CPU transition path remains the
  null/headless implementation and the pixel oracle.
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
* **The timer's fire boundary is a whole millisecond, the reference's
  is a sub-millisecond one.** The reference's timer thread compares the
  deadline against the wall clock in sub-millisecond units and fires when the
  clock is *strictly* past the deadline
  (`reference/cpp/core/utils/impl/TimerImpl.cpp:128`); the port's timer sees
  the engine clock's whole-millisecond value and fires at the deadline. The
  two agree everywhere except when the elapsed time lands exactly on a
  deadline, which no real frame sequence does — 100 000 frames of 16.6 ms
  never hit one. Closing it would mean handing the registry a fractional
  time, and the registry's whole-millisecond interface is the L2 surface.

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


* **An archive member that carries the game's text cipher is read as text.**
  The cipher container is the three-byte signature `FE FE <mode>`, the
  mandatory UTF-16LE mark and then the payload
  (`reference/cpp/core/base/TextStream.cpp:271-280`), and the port decodes it
  in the text layer (`docs/code/compat/text.md`); the reference's text
  *reader* takes the payload one byte earlier
  (`reference/cpp/core/base/TextStream.cpp:87-89`), so such a member decodes to
  shifted garbage there. The XP3 filter is a different mechanism and is
  registered only when the application ships `xp3filter.tjs`
  (`reference/cpp/plugins/xp3filter.cpp:550-564`).
## 18. Image-pipeline follow-ups

The deferred image pool, its L3 wiring, the cache insert and the startup
counters are landed. The whole workspace suite and the static gate are green.
The six earlier failures are closed:

* the motion mesh test now evaluates the previous-frame patch in normalized
  box coordinates instead of player-pixel coordinates;
* `dispatch_ui`, `xp3_filter`, `xp3_filter_deferred` and `wuvorbis`
  link the lazy plugin that owns the member or decoder they use
  (`windowEx.dll`, `win32dialog.dll`, `fstat.dll`, `wuvorbis.dll`,
  `wutcwf.dll`) before they create the object or open the stream — lazy
  registration is the reference behavior, not a test workaround;
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
* The copy family's arithmetic **below the full opacity** is an
  approximation. The reference's method table gives every copy method a
  full-opacity spelling and a below-full-opacity spelling
  (`reference/cpp/core/visual/RenderManager.cpp:2369-2399`): `bmCopy` runs
  `Copy` / `CopyColor` / `ConstAlphaBlend` and `bmCopyOnAlpha` runs
  `CopyOpaqueImage` / `CopyColor` / `ConstAlphaBlend_d`. The tree's own
  `ConstAlphaBlend` bodies are not vendored (only the call sites in
  `reference/cpp/core/visual/LayerBitmapIntf.cpp:1600-1606` are), so the
  arithmetic below the full opacity cannot be read from the oracle. The port
  keeps its established `ConstAlpha` kernel, which holds the destination's
  alpha where the tree's GL spelling writes the source's. The same gap
  covers the operate family's copy mode: the reference hands the hold flag
  to the blend at every opacity, but its below-full-opacity spelling for the
  copy methods is the same with and without the hold, so the flag has no
  effect there. Recorded as an approximation with its reference gap, not as a
  verified match.

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

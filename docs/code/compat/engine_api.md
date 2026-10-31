# `engine_api.rs` — the L3 ↔ L2 seam

**Source:** `crates/compat/src/engine_api.rs`

This file is **the only** file under `crates/compat/src` allowed to name the
`engine` crate. The `l3-engine-api` rule inside the `layering` gate check
rejects `engine::` anywhere else under `compat/src`, and its self-test proves
the rule fires on a bad fixture and leaves a near-miss clean.

The seam is a **re-export, not a wrapper**: a protocol type *is* the engine
type, so `CompatHost`/`CompatServices`' public signatures, the script surface
and the surface lock are unchanged.

The full inventory of the engine side — what each item owns, its coordinate
space, its error type and its lifetime — is
[`../../arch/l2.md`](../../arch/l2.md) §10. The rules the seam obeys are in
[`../../rules/api_rules.md`](../../rules/api_rules.md) §4 and
[`../../rules/arch_rules.md`](../../rules/arch_rules.md) §11.

---

## 1. What crosses the seam

Every item below is L2 **state** or an L2 **action on that state**. There is
no L1 service here: L3 never names a `FileSystem`, `RenderPipeline`, `Media`
or `FontSystem` — it reaches the platform only through L2.

| Group | Items | What L3 uses them for |
|---|---|---|
| the logical scene | `Scene`, `LayerState`, `BitmapState`, `FontState`, `Rect`, `DecodedImageKey`, `DecodedImageMode`, `TransitionFrame`, `HitCandidate` | the layer tree, the bitmap store, the font store, the decoded-image cache, the frame a composited transition draws |
| the storage namespace | `Storage`, `Location`, `StorageReadError`, `StorageMedia`, `normalize_in_archive_name` | mounting, resolving and reading a script storage name, and serving the names a storage media owns |
| input | `InputState` | the generic pointer, key, button and touch state |
| timers | `TimerState`, `TimerId`, `TimerCallback` | the L2 timer registry L3 arms and disarms |
| the frame clock | `FrameClock` | the engine's time base: the value the timers, the sound poll and the transitions read |
| the clock source | `FrameTimeSource` | the one time base: the frame samples it at each boundary, `System.getTickCount` samples it live |
| text | `TextEngine`, `TextStyle`, `TextError`, `FontId`, `TextCoverageCache`, `CoverageMask`, `rasterize_text_coverage`, `blit_coverage_into`, `blur_coverage`, `blurred_text_coverage`, `compose_text_into` | shaping, coverage rasterization, the glyph cache |
| sound | `Mixer`, `SoundBuffer`, `SoundChannel`, `SoundSource`, `SoundEvent`, `SoundLabel`, `SoundError`, `LoopInfo`, `LoopLink`, `LoopCondition`, `Status`, `AudioStream`, `LOOP_FLAG_COUNT` | the shared mixer and its channels, buffers and sources |
| video | `VideoOverlay`, `VideoBounds`, `VideoEvent`, `PlayStatus`, `blit_frame_to_layer`, `blit_frame_to_layer_opacity` | the playback machine and the frame blits |
| transitions | `Trans`, `TransKind`, `TransError`, `RuleMask`, `ScrollFrom`, `ScrollStay`, `DestAlpha` | the transition state machine, its typed errors, the rule/scroll vocabulary and the destination surface's alpha handling: the transition mix and the blit kernels are keyed by it |
| `raster` | `blend_pixel`, `fill_polygon`, `fill_rect_replace`, `flatten_arc`, `flatten_cubic`, `hatch_foreground` | the rasterization primitives L3 composes directly |
| `layer_ops` | `AffineBlit`, `BlitOptions`, `BlitSource`, `GammaAdjust`, `RectI`, `COPY_MAIN`, `COPY_MASK`, `adjust_gamma`, `affine_blit`, `affine_blit_clipped`, `affine_blit_clipped_on`, `affine_blit_clipped_pixel_center`, `affine_blit_clipped_pixel_center_on`, `blit_copy`, `blit_over`, `blit_plane`, `blur_bitmap_in_place`, `clip_rect`, `convert_add_alpha_to_alpha`, `convert_alpha_to_add_alpha`, `copy_9patch`, `do_gray_scale`, `ensure_transition_display`, `ensure_transition_self_image`, `fill_color_hold_alpha`, `fill_color_on_add_alpha`, `fill_color_on_alpha`, `fill_mask`, `flip_lr`, `flip_ud`, `gaussian_blur`, `intersect`, `intersect_rect`, `light_contrast`, `operate_rect`, `operate_rect_on`, `remove_const_opacity`, `render_layer_subtree`, `stretch_blit_mode`, `stretch_blit_on`, `stretch_blit_plane` | the layer composition and pixel-region operations |

The `raster` and `layer_ops` groups are nested modules so a call site reads
`engine_api::layer_ops::blit_over` rather than a flat import list.

---

## 2. The rules the seam obeys

* **No L1 service appears in it.** L3 never names a `FileSystem`,
  `RenderPipeline`, `Media` or `FontSystem`.
* **Every item is L2 state or an L2 action on that state.** There is no
  behaviour in the seam — only names.
* **Coordinates are VM pixels only.** L3 never calls `to_device_*`,
  `to_vm_point` or `raster_scale`, and never names a device pixel. The
  translation between VM and device pixels lives inside L2; see
  [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §5.
* **Errors are typed values** mapped at the call site. `Option` means "the
  object does not exist"; `()` means "the primitive is infallible on
  validated VM bounds".
* **TVP vocabulary stays out of L2.** The native blend numbering (`lt*`),
  the `0..100000` volume scale, the `.sli` grammar and the KAG tags are L3;
  L2 keeps `BlendMode`, a normalised gain and an opaque key code.
* **A storage media is L3's own.** `StorageMedia` is the provider contract L2
  dispatches a `<scheme>://…` name to. L3 implements it — the `var://` media
  walks the script object graph, the `psb://` media resolves a member path
  against a parsed container — and L2 holds the registered providers without
  knowing what a scheme's domain is made of. A provider's message crosses the
  seam unchanged (`StorageReadError::Media`): the domain's own wording is
  what the script sees.

---

## 3. Adding or removing an item

**Adding:** add or reuse it in `engine` with an L2-generic name, re-export it
in the correct group here, use it from L3 through `crate::engine_api`, and
record it in [`../../arch/l2.md`](../../arch/l2.md) §10.

**Removing:** delete it from the engine, from this seam and from every call
site in the same change. No compatibility shim is kept.

---

## 4. How the handles reach the seam

The seam names the **types**; the values arrive through the four handles the
host attaches to `CompatServices` ([`ctx.md`](ctx.md)):

| Handle | Type | Attached by |
|---|---|---|
| scene | `*mut Scene` | `CompatServices::attach_scene` |
| storage | `*mut Storage` | `CompatServices::attach_storage` |
| input | `*mut InputState` | `CompatServices::attach_input` |
| timer | `*mut TimerState` | `CompatServices::attach_timer` |
| clock | `*mut FrameClock` | `CompatServices::attach_clock` |
| clock source | `*const dyn FrameTimeSource` | `CompatServices::attach_frame_time` |

A native that needs the scene takes the raw pointer from the services and
dereferences it under its own `unsafe` contract — see
[`visual/common.md`](visual/common.md) for the pattern the visual family uses.

---

## 5. Tests

The seam has no test of its own; it is a list of names. What is tested is
that the **rule** holds and that the composed behaviour is right:

| Check / test | What it proves |
|---|---|
| `krkr-gate layering` (`l3-engine-api` rule) | no other file under `compat/src` names `engine::` |
| `crates/compat/tests/layer_behaviour.rs` | the L2 blend mapping round-trips every native spelling |
| `crates/compat/tests/visual.rs` | the scene state a native reads and writes is the L2 state |
| `crates/compat/tests/timer_behavior.rs` | the L2 timer schedule is armed and disarmed exactly as the script asks |
| `crates/compat/tests/sound.rs` | the mixer, its channels and the script scale behave as the reference does |

---

## See also

* [`ctx.md`](ctx.md) — the handles the seam's types arrive through
* [`../../arch/l2.md`](../../arch/l2.md) §10 — the protocol inventory on the engine side
* [`../../rules/api_rules.md`](../../rules/api_rules.md) §4 — the protocol rules

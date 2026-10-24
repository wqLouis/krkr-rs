# `compat` — the L3 compatibility surface, module by module

This directory documents `crates/compat` **as the code is**: one page per
module, each stating what the module owns, what a script sees, which
invariants it enforces, what it raises, and which tests pin it.

The layer-level contract — the four layers, the dependency direction, the
state-ownership rule, the surface lock — lives elsewhere and is not repeated
here:

| Page | Covers |
|---|---|
| [`../../arch/overall.md`](../../arch/overall.md) | the four layers, the crate map, the coordinate spaces, the frame |
| [`../../arch/l3.md`](../../arch/l3.md) | the L3 contract: the engine seam, the native classes, plugins, events, the frame body, the known gaps |
| [`../../arch/l2.md`](../../arch/l2.md) | the L2 engine the compatibility surface composes, and the L3↔L2 protocol inventory |
| [`../../rules/arch_rules.md`](../../rules/arch_rules.md) | the rules: layers, state, frame, coordinate spaces, errors |
| [`../../rules/api_rules.md`](../../rules/api_rules.md) | the rules: public surfaces, the protocol, ownership, error enums |
| [`../../rules/code_styles.md`](../../rules/code_styles.md) | the Rust reviewer checklist |
| [`../../../AGENTS.md`](../../../AGENTS.md) | the working contract for every agent that edits this tree |

Design history, the decision log and the deep reference evidence trail live in
[`../../../docs.old/migration/`](../../../docs.old/migration/) — read it when
you need the *why*, not the *what*.

---

## 1. The pages

### Core

| Page | Module | One line |
|---|---|---|
| [`binding.md`](binding.md) | `binding.rs` | the one process-wide engine → context table the C ABI forces |
| [`host.md`](host.md) | `host.rs` | `CompatHost` — the dispatch state owner and the install/uninstall lifecycle |
| [`ctx.md`](ctx.md) | `ctx.rs` | `CompatCtx` + `CompatServices` — the registration/dispatch context and the L2 service handles |
| [`engine_api.md`](engine_api.md) | `engine_api.rs` | the single L3↔L2 seam; the only file allowed to name `engine::` |
| [`native_util.md`](native_util.md) | `native_util.rs` | the shared marshalling helpers every raw callback uses |
| [`resource.md`](resource.md) | `resource.rs`, `storage.rs` | per-engine face resolution, the host image codec, pre-rendered fonts, and the script-name → VFS-name boundary (the `file://` media prefix) |
| [`text.md`](text.md) | `text.rs` | script decoding: BOM, charset detection, the `FE FE` container |
| [`event.md`](event.md) | `event.rs` | `EventAdapter` — platform events → TJS events → script handlers |
| [`frame.md`](frame.md) | `frame.rs` | `FrameBody` and `run_frame_body` — the ordered per-frame poll body |

### Plugins

| Page | Module | One line |
|---|---|---|
| [`plugin.md`](plugin.md) | `plugin.rs` | `NativePlugin`, `PluginRegistry`, the built-in table |
| [`null_plugin.md`](null_plugin.md) | `null_plugin.rs` | the null plugin object for a DLL that is not ported |
| [`plugins_native.md`](plugins_native.md) | `plugins_native.rs` | the `Plugins` native class — `link` / `unlink` / `getList` |

### The native classes

| Page | Module | One line |
|---|---|---|
| [`natives.md`](natives.md) | `natives/` | the non-visual families: `System`, `Storages`, `Scripts`, `Mouse`/`Key`/`Pad`, sound, video, `AsyncTrigger`, `MenuItem`, `Debug`, `Rect`, `ImageFunction`, `Clipboard`, `BitmapLayerTreeOwner`, `KAGParser` |
| [`visual.md`](visual.md) | `natives/visual/` | the visual families and the scene they compose |

### The per-family pages

Each native family also has its own page, with the full member table and
the family's own state ownership:

| Page | Class |
|---|---|
| [`system.md`](system.md) | `System` |
| [`storages.md`](storages.md) | `Storages` |
| [`scripts.md`](scripts.md) | `Scripts` |
| [`input.md`](input.md) | `Mouse`, `Key`, `Pad` |
| [`sound.md`](sound.md) | `WaveSoundBuffer`, `WaveFlags`, `PhaseVocoder` |
| [`cdda_midi.md`](cdda_midi.md) | `CDDASoundBuffer`, `MIDISoundBuffer` |
| [`video.md`](video.md) | `VideoOverlay` |
| [`async_trigger.md`](async_trigger.md) | `AsyncTrigger` |
| [`menu_item.md`](menu_item.md) | `MenuItem` |
| [`debug.md`](debug.md) | `Debug`, `Controller`, `Console` |
| [`rect.md`](rect.md) | `Rect` |
| [`image_function.md`](image_function.md) | `ImageFunction` |
| [`clipboard.md`](clipboard.md) | `Clipboard` |
| [`tree_owner.md`](tree_owner.md) | `BitmapLayerTreeOwner` |
| [`tjsnum.md`](tjsnum.md) | the `string → integer` coercion |
| [`kag.md`](kag.md) | `KAGParser` — with [`kag/parser.md`](kag/parser.md), [`kag/state.md`](kag/state.md) and [`kag/kvp.md`](kag/kvp.md) |

### The plugin pages

| Page | Covers |
|---|---|
| [`plugins.md`](plugins.md) | the plugin architecture and the whole ported set |
| [`plugins/`](plugins/) | one page per ported plugin: `addFont`, `csvParser`, `dirlist`, `emotePlayer`, `extrans`, `fftGraph`, `fstat`, `getAbout`, `getSample`, `json`, `layerEx`, `motionPlayer`, `psbFile`, `psdFile`, `saveStruct`, `scriptsEx`, `simpleBinder`, `steam`, `textRender`, `varFile`, `win32Dialog`, `windowEx`, `wutcwf`, `wuvorbis`, `xp3Filter` |

### The visual modules

| Page | Module | One line |
|---|---|---|
| [`visual/common.md`](visual/common.md) | `natives/visual/common.rs` | shared dispatch state, value marshalling, object ↔ scene-id resolution |
| [`visual/window.md`](visual/window.md) | `natives/visual/window.rs` | the `Window` class |
| [`visual/layer.md`](visual/layer.md) | `natives/visual/layer/mod.rs`, `order.rs` | the `Layer` class: construction, geometry, events, ordering, modal mode |
| [`visual/focus.md`](visual/focus.md) | `natives/visual/layer/focus.rs` | the focus domain model and the blur-off properties |
| [`visual/pixels.md`](visual/pixels.md) | `natives/visual/layer/pixels.rs` | the pixel operations, the image load/save path, the text path |
| [`visual/transitions.md`](visual/transitions.md) | `natives/visual/layer/transitions.rs` | `beginTransition`, the transition pumps, the script cache |
| [`visual/bitmap.md`](visual/bitmap.md) | `natives/visual/bitmap.rs` | the `Bitmap` class; the image codec it consumes is installed through `CompatHost::set_image_codec` |
| [`visual/font.md`](visual/font.md) | `natives/visual/font.rs` | the `Font` class and face resolution |
| [`visual/timer.md`](visual/timer.md) | `natives/visual/timer.rs` | the `Timer` class and the L2 timer schedule |
| [`visual/draw_device.md`](visual/draw_device.md) | `natives/visual/draw_device.rs` | `BasicDrawDevice` / `PassThroughDrawDevice` |

### The crate-root public API

Everything an L4 host (or a test) can reach is re-exported from `lib.rs`,
and nothing else is public. The groups:

| Group | Items |
|---|---|
| the host and its context | `CompatHost`, `CompatCtx`, `CompatServices`, `CompatError` |
| the frame body | `FrameBody`, `run_frame_body` |
| the event adapter | `EventAdapter`, `EventValue`, `TjsEvent` |
| the per-frame polls | `timer_poll`, `sound_poll`, `sound_render`, `video_advance`, `async_trigger_poll`, `continuous_handler_poll`, `movie_poll` |
| the tree-owner hand-offs | `advance_transitions`, `advance_self_updating_transitions`, `paint_poll`, `refresh_cached_layers` |
| the host seams | `VideoSource`, `set_video_source`, `set_movie_source`, `AudioDecoder`, `add_audio_decoder`, `set_output_latency`, `set_sound_render_driven` |
| the request drains | `take_exit_request`, `take_bring_to_front_requests`, `take_update_requests`, `take_capture_releases` |
| the startup script | `exec_storage` |
| the plugin architecture | `NativePlugin`, `PluginOutcome`, `PluginRegistry`, `normalize_plugin_name`, `plugin_stem`, `builtin_plugin_names` |

Every one of them states its ownership, its error contract and its panic
and FFI-safety rules in its own rustdoc. The L2 seam (`engine_api.rs`) and the
per-family modules stay crate-internal: a native family reaches the engine
only through the seam, and the host reaches a family only through the
re-exports above.

---

## 2. The one process-wide binding

The C++ ABI hands a native callback only the raw `tjs2_engine*`, and `tjs`
exposes **no user-data slot**. `compat` therefore keeps exactly **one**
process-wide table that maps that pointer back to the Rust engine, the L2
services and the plugin registry.

It is L3 (never L1), it holds *dispatch* state and never game state, and the
host installs it before any native callback can run and clears it before the
engine is dropped. Details: [`binding.md`](binding.md).

Every native callback resolves its context through that table; there is no
second global anywhere in the crate. Per-family state that outlives a callback
(the `Timer` bindings, the `MenuItem` tree, the sound objects, the debug log
ring) lives in the `CompatServices` state map — see [`ctx.md`](ctx.md).

---

## 3. The dispatch surface is the contract

Everything the VM can reach — which classes exist and which members they
expose — is frozen by `crates/compat/tests/runtime_surface.rs`, which asks the
**live** VM what `CompatHost::install` actually registered and compares the
dump against [`../../surface/runtime_surface.json`](../../surface/runtime_surface.json):

```bash
uv run --project scripts krkr-gate surface-lock            # regenerate + check
uv run --project scripts krkr-gate surface-lock --update   # accept a change (main agent only)
```

Internals are free to change while that dump is unchanged; adding or removing
a member is a reviewed baseline change. The two parity reports —
[`../../native_parity.md`](../../native_parity.md) (core classes) and
[`../../plugin_parity.md`](../../plugin_parity.md) (plugin classes) — answer
"what is missing?" and stay reports; the lock answers "what changed?".
Behaviour is constrained separately: every semantic is cited to
`reference/cpp/**` in the test that asserts it.

---

## 4. What every module in this crate does

* **Marshals arguments, calls one L2 operation, marshals the result.** A
  member that contains a loop, a state machine, blending, timing or
  scheduling is in the wrong layer — see
  [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §3.
* **Names the engine only through `crate::engine_api`.** The `layering` gate
  check rejects `engine::` anywhere else under `compat/src`.
* **Raises a typed TJS error where the reference raises.** There is no
  fabricated default on a fallible path. The single documented exception is
  the null plugin object ([`null_plugin.md`](null_plugin.md)).
* **Resolves event members on the object**, so a script subclass override
  wins over the native default (`tests/extend_dispatch.rs`).
* **Holds no platform code**: no `#[cfg(target_os = …)]`, no `std::fs`, no
  JNI, no winit. Platform facts arrive as host-installed closures.

---

## 5. Conventions used in these pages

| Section | What it holds |
|---|---|
| *Source* | the file(s) the page describes |
| *State ownership* | which state is L2 engine state, which is the L3 native payload, which is L3 dispatch state |
| *Script-visible behaviour* | what a script observes, member by member |
| *Invariants* | the rules the code enforces |
| *Errors* | the exact message text and when it is raised |
| *Host seams* | what the composition root must install or call |
| *Tests* | the file and the test names that pin the behaviour |

Error messages are quoted verbatim from the source because a script can
observe them; reference citations live in the tests, as
[`../../../AGENTS.md`](../../../AGENTS.md) §8 requires.

---

## 6. Known gaps

The aggregate list of open behaviour gaps in this layer is
[`../../arch/l3.md`](../../arch/l3.md) §9. Module-scoped divergences are
recorded on the page that owns them (`Storages.getPlacedPath` returns a
relative path, `WaveSoundBuffer.bits` is always 32, `System.inform` logs
instead of showing a dialog, and so on).

Where a member's behaviour is known to differ from the reference, the page
says so and names the reference gap. A gap that is **not** named on a page is
not a claim that the behaviour is exact — it means no test asserts it yet.

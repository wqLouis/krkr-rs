# The code map — one page per module

Source: `crates/**` · the whole workspace

This directory documents the workspace **as the code is**: one page per module
(or per cohesive group of modules), each stating what the module owns, what it
exposes, which invariants it enforces, what it raises, and which tests pin it.

The layer-level contract — the four layers, the dependency direction, the
state-ownership rule, the surface lock — is **not** repeated here. It lives
in the two sibling directories:

| Directory | Holds | Read it when |
|---|---|---|
| [`../arch/`](../arch/overall.md) | the architecture: the layers, the crate map, the coordinate spaces, the frame, the L3↔L2 protocol, and the [L2 public API reference](../arch/l2-api.md) | you need to know *why* a module is shaped the way it is, or *what* the engine crate offers a caller |
| [`../rules/`](../rules/arch_rules.md) | the rules: layers, state, frame, coordinate spaces, errors, the Rust review checklist | you are reviewing or writing code |
| [`../README.md`](../README.md) | the index of the whole documentation tree | you are looking for a page |

Design history, the decision log, the audits and the deep reference evidence
trail live in the archive, [`../../docs.old/`](../../docs.old/) — read it when
you need the *why* behind a decision, not the *what* of the current code. The
current contract for a topic is always a page under `arch/`, `code/` or
`rules/`.

---

## 1. The directories

| Directory | Crates | Layer |
|---|---|---|
| [`platform/`](platform/README.md) | `api`, `fs`, `media`, `psd`, `render`, `platform` | L1 — the stateless platform core |
| [`engine/`](engine/README.md) | `engine` | L2 — the middle layer: all evolving state |
| [`compat/`](compat/README.md) | `compat` | L3 — the KiriKiri compatibility surface |
| [`vm.md`](vm.md), [`tjs.md`](tjs.md) | `vm`, `tjs` | L4 — the TJS VM host and the vendored VM |
| [`app/`](app/README.md) | `app` | root — the desktop composition root |
| [`android/`](android/README.md) | `android` | root — the Android composition root |
| [`testkit.md`](testkit.md) | `testkit` | dev-only — the shared test helpers |

The dependency direction is strict and one-way: **L4 → L3 → L2 → L1**, with
the composition root above everything. No layer may call up.

---

## 2. L1 — the platform core

| Page | Crate | Covers |
|---|---|---|
| [`platform/README.md`](platform/README.md) | `crates/platform/*` | the object/state split across the L1 crates |
| [`platform/api.md`](platform/api.md) | `api` | the shared DTOs and the typed error enums |
| [`platform/fs.md`](platform/fs.md) | `fs` | the file system, the file state, the XP3 container parse |
| [`platform/media.md`](platform/media.md) | `media` | the image, audio and video decoders and the encoders |
| [`platform/media/psd.md`](platform/media/psd.md) | `psd` | the PSB container and the PSD image decoder |
| [`platform/render.md`](platform/render.md) | `render` | the render pipeline, the Vulkan backend, the font system |
| [`platform/platform.md`](platform/platform.md) | `platform` | the window, the event loop, the clock, the audio device, the metrics |

---

## 3. L2 — the engine

| Page | Module | Covers |
|---|---|---|
| [`engine/README.md`](engine/README.md) | `engine` | the crate at a glance: the state it owns, the services it calls |
| [`engine/engine.md`](engine/engine.md) | `engine.rs` | the frame-owning object: the clock, the timer pump, the projection, the presentation |
| [`engine/clock.md`](engine/clock.md) | `clock.rs` | the frame clock: the time base the frame advances and the script reads |
| [`engine/scene.md`](engine/scene.md) | `scene.rs` | the logical layer tree and the per-layer state |
| [`engine/projection.md`](engine/projection.md) | `projection.rs` | the L2 → L1 seam: the `RenderScene` the pipeline draws |
| [`engine/layer_ops.md`](engine/layer_ops.md) | `layer_ops.rs` | the pixel primitives and the subtree compositing |
| [`engine/raster.md`](engine/raster.md) | `raster.rs` | the CPU rasterizer: fills, gradients, hatching |
| [`engine/storage.md`](engine/storage.md) | `storage.rs` | the storage namespace: the mounts, the precedence, the resolution order |
| [`engine/startup.md`](engine/startup.md) | `startup.rs` | the startup discovery and the script plan |
| [`engine/sound.md`](engine/sound.md) | `sound.rs` | the mixer, the channels, the fades and the loops |
| [`engine/video.md`](engine/video.md) | `video.rs` | the video-overlay playback machine |
| [`engine/trans.md`](engine/trans.md) | `trans/` | the transition state machine and the `extrans` providers |
| [`engine/shader.md`](engine/shader.md) | `shader/` | the shader-program registry |
| [`engine/text.md`](engine/text.md) | `text.rs` | the text engine: the layout, the shaping and the glyph atlas |
| [`engine/timer.md`](engine/timer.md) | `timer.rs` | the timer registry the frame advances |
| [`engine/input.md`](engine/input.md) | `input.rs` | the per-frame input state |
| [`engine/translate.md`](engine/translate.md) | `translate.rs` | the VM ↔ device pixel translation |

---

## 4. L3 — the compatibility surface

| Page | Covers |
|---|---|
| [`compat/README.md`](compat/README.md) | the crate at a glance and the index of every module page |
| [`compat/binding.md`](compat/binding.md) | the one process-wide engine → context table |
| [`compat/host.md`](compat/host.md) | `CompatHost`: the dispatch state owner and the install lifecycle |
| [`compat/ctx.md`](compat/ctx.md) | `CompatCtx` and `CompatServices` |
| [`compat/engine_api.md`](compat/engine_api.md) | the single L3 ↔ L2 seam |
| [`compat/plugin.md`](compat/plugin.md) | `NativePlugin`, `PluginRegistry`, the built-in table |
| [`compat/plugins.md`](compat/plugins.md) | the plugin architecture and the whole ported set |
| [`compat/plugins_native.md`](compat/plugins_native.md) | the `Plugins` native class |
| [`compat/natives.md`](compat/natives.md) | the non-visual native families |
| [`compat/visual.md`](compat/visual.md) | the visual native families and the scene they compose |
| [`compat/frame.md`](compat/frame.md) | `FrameBody` and the ordered per-frame poll body |
| [`compat/event.md`](compat/event.md) | the platform event → TJS event adapter |
| [`compat/resource.md`](compat/resource.md) | the per-engine face resolution and the host image codec |
| [`compat/kag.md`](compat/kag.md) | the KAG parser and the tag state machine |
| [`compat/plugins/`](compat/plugins/) | one page per ported plugin (25 pages) |

The per-class pages (`System`, `Storages`, `Scripts`, `WaveSoundBuffer`,
`VideoOverlay`, `Layer`, `Window`, `Bitmap`, `Font`, `Timer`, …) and the
per-plugin pages are indexed in [`compat/README.md`](compat/README.md).

---

## 5. L4 — the VM host

| Page | Crate | Covers |
|---|---|---|
| [`vm.md`](vm.md) | `vm` | the TJS VM host: the engine wrapper, the script cache, the frame body |
| [`tjs.md`](tjs.md) | `tjs` | the vendored C++ TJS2 VM and its FFI surface |

---

## 6. The composition roots

| Page | Crate | Covers |
|---|---|---|
| [`app/README.md`](app/README.md) | `app` | the desktop root: the wiring, the loop, the host |
| [`app/main.md`](app/main.md) | `app` | the CLI, the launch options, the entry point |
| [`app/game.md`](app/game.md) | `app` | `GameHost`: the game loop, the frame body, the slow-frame attribution |
| [`app/audio.md`](app/audio.md) | `app` | the decoder seam and the output device |
| [`app/video.md`](app/video.md) | `app` | the video source seam |
| [`app/overlay.md`](app/overlay.md) | `app` | the render-only debug overlay |
| [`app/config.md`](app/config.md) | `app` | the runtime configuration and its overrides |
| [`android/README.md`](android/README.md) | `android` | the Android root at a glance |
| [`android/host.md`](android/host.md) | `android` | the `GameActivity` event loop and the JNI seam |
| [`android/input.md`](android/input.md) | `android` | the touch bridge |
| [`android/gesture.md`](android/gesture.md) | `android` | the gesture recognizer |
| [`android/config.md`](android/config.md) | `android` | the launcher settings |
| [`android/ui.md`](android/ui.md) | `android` | the launcher's Material 3 UI: the navigation rail, the three destinations, the library, the settings and the logs |
| [`android/build.md`](android/build.md) | `android` | the Gradle packaging and the CI build |

---

## 7. Dev-only

| Page | Crate | Covers |
|---|---|---|
| [`testkit.md`](testkit.md) | `testkit` | the shared test helpers (a dev-dependency only) |
| [`devtools.md`](devtools.md) | `devtools` | the developer tools: the XP3 inspector and the TJS bytecode disassembler |

---

## 8. Conventions used in these pages

| Section | What it holds |
|---|---|
| *Source* | the file(s) the page describes |
| *State ownership* | which state is L1 service state, which is L2 engine state, which is L3 dispatch state, which is L3 payload |
| *Script-visible behaviour* | what a script observes, member by member |
| *Invariants* | the rules the code enforces |
| *Data flow* | the order the values travel in |
| *Failure modes* | the exact error type or message, and when it is raised |
| *Current limits* | what is **not** done yet, stated as a limit and never as a behaviour |
| *Tests* | the file and the test names that pin the behaviour |

Error messages are quoted verbatim from the source because a script can
observe them; reference citations live in the tests, as
[`../../AGENTS.md`](../../AGENTS.md) §8 requires. A page that says nothing
about a behaviour is **not** a claim that the behaviour is exact — it means no
test asserts it yet.

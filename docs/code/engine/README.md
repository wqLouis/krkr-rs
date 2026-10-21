# `engine` — the middle layer (L2)

Source: `crates/engine/src/` · crate `engine` · layer **L2**

`engine` is the small, generic game engine between the KiriKiri compatibility
surface (L3, `compat`) and the platform core (L1, `crates/platform/*`). It
owns **every piece of evolving state** the game has — the layer tree, the
storage namespace, the mixer, the video overlay, the timer registry, the input
state, the text engine — and performs actions on the stateless L1 services
(`RenderPipeline`, `FileSystem`, `FontSystem`).

It speaks no TJS and no TVP vocabulary of its own: everything a script sees is
expressed by L3 on top of the generic values documented here.

The architecture this crate implements is in [`arch/l2.md`](../../arch/l2.md);
the rules it obeys are in [`rules/arch_rules.md`](../../rules/arch_rules.md)
(state ownership, frame contract, coordinate spaces), [`rules/api_rules.md`](../../rules/api_rules.md)
(public surface, the L3 ↔ L2 protocol, error enums) and
[`rules/code_styles.md`](../../rules/code_styles.md) (the Rust review
checklist). This directory is the **code map**: what each module owns, its
public API, its invariants, its data flow, its failure modes and its tests.

---

## 1. The crate at a glance

| | |
|---|---|
| Layer | L2 (middle layer) |
| Depends on | `api` (L1 DTOs + error enums), `fs` (L1 file system + XP3 parse), `render` (L1 render pipeline + font system), `cosmic-text`, `thiserror`, `log`, `ahash` |
| Must not depend on | `compat` (L3), `vm`/`tjs` (L4), any platform crate, `winit`, `ash`, FFmpeg/MediaCodec, `cpal`, `std::fs` |
| Lints | `#![forbid(unsafe_code)]`, `#![warn(missing_docs)]` |

The engine depends on the L1 **type** crates and may name their types in its
public API (`RenderScene`, `FileState`, `FontMetrics`, …); it does not depend
on `media`, `platform` or `tjs`.

---

## 2. Module map

One document per module. Each states the module's responsibility, the state
it owns, its public API, its invariants, its data flow, its failure modes and
its tests.

| module | source | one-line role | document |
|---|---|---|---|
| `engine` | `src/engine.rs` | the frame-owning object: scene, projection, textures, timers, presentation | [engine.md](engine.md) |
| `scene` | `src/scene.rs` | the logical layer tree: windows, layers, bitmaps, fonts, focus domains | [scene.md](scene.md) |
| `imagedecode` | `src/imagedecode.rs` | the deferred image decode pool and its pending-image future | [scene.md](scene.md#the-pending-image) |
| `projection` | `src/projection.rs` | the pure field copy from the logical tree to the L1 `RenderScene` | [projection.md](projection.md) |
| `translate` | `src/translate.rs` | the VM-pixel ↔ device-pixel mapping (Fit with letterbox) | [translate.md](translate.md) |
| `layer_ops` | `src/layer_ops.rs` | in-place CPU pixel operations on bitmaps (fills, blits, blur, subtree compositing) | [layer_ops.md](layer_ops.md) |
| `raster` | `src/raster.rs` | the CPU shape rasterizer (polygons, strokes, hatches, arcs, béziers) | [raster.md](raster.md) |
| `text` | `src/text.rs` | shaping, layout, coverage rasterization, glyph atlas, text projection | [text.md](text.md) |
| `storage` | `src/storage.rs` | the storage namespace: mounts, resolution order, reads, listings | [storage.md](storage.md) |
| `startup` | `src/startup.rs` | startup-script discovery | [startup.md](startup.md) |
| `sound` | `src/sound.rs` | the sound model and the software mixer | [sound.md](sound.md) |
| `video` | `src/video.rs` | the video-overlay playback machine | [video.md](video.md) |
| `trans` | `src/trans/` | the transition state machine and the `extrans` providers | [trans.md](trans.md) |
| `shader` | `src/shader/` | the shader-program registry: the L2 half of the custom-shader contract | [shader.md](shader.md) |
| `timer` | `src/timer.rs` | the timer registry | [timer.md](timer.md) |
| `input` | `src/input.rs` | the generic input state | [input.md](input.md) |
| — | `src/hasher.rs` | the fixed-seed hash map used by the crate's lookup tables | — |

---

## 3. The crate root's public surface

`lib.rs` re-exports a **curated** subset; the rest of each module stays
reachable through the module path.

```rust
pub use api::config::CacheLimit;
pub use fs::xp3::normalize_in_archive_name;

pub use crate::engine::{
    Engine, EngineError, EngineOverlay, EngineParts, FrameReport, GameFrame, ObjectCounts,
};
pub use crate::hasher::Map;
pub use crate::translate::{PixelTranslation, TranslateError};

pub use projection::{
    ComposedLayer, ProjectionReport, compose_layer, project_scene, project_scene_report,
    project_window, project_window_report, resolve_blend,
};
pub use scene::{
    BitmapState, DecodedImageKey, DecodedImageMode, FontState, LayerState, Rect, Scene,
    TransitionFrame, TransitionSource, WindowState,
};
pub use shader::{LoadedProgram, ProgramDesc, ShaderError, ShaderId, ShaderLibrary};
pub use startup::{StartupError, discover_startup};
pub use storage::{Location, MountError, Storage, StorageMetadata, StorageReadError};
```

The pixel primitives, the blend-type numbering and the storage/startup
constants are **not** re-exported flat: they are reached through
`engine::layer_ops`, `engine::raster`, `engine::projection`, `engine::scene`,
`engine::storage` and `engine::startup`.

`engine::Map` is the crate's `HashMap` with a **fixed-seed** hasher, so the
lookup tables key deterministically across runs (`ahash`'s default
`RandomState` is randomized per process).

---

## 4. What crosses the L3 seam

The L3 ↔ L2 protocol is a **seam**: `crates/compat/src/engine_api.rs` is the
only file under `crates/compat/src` allowed to name `engine::`, and the
`l3-engine-api` rule of the `layering` gate check enforces it. The items that
cross it are listed in [`rules/api_rules.md`](../../rules/api_rules.md) §4 and
in [`arch/l2.md`](../../arch/l2.md) §10.

Two things are deliberately **outside** the seam: the projection and the draw
tree (`RenderScene`, `RenderState`, `Surface`) — L2 projects, L1 draws, and
L3 is not on that path at all — and `PixelTranslation` with its
`to_device_*` / `to_vm_point` / `raster_scale` methods, which stay L2-internal.

---

## 5. Coordinates

| space | unit | owner | crosses L3 ↔ L2? |
|---|---|---|---|
| **VM pixel** | one pixel of the logical screen (the primary layer) | L3 and scripts | yes — every protocol value is VM |
| **window pixel** | one pixel of the OS window's client area | L3 and scripts | yes (`Window.innerWidth`/`innerHeight`) |
| **device pixel** | one pixel of the framebuffer/swapchain | L1 | no |

L2 owns the Fit mapping (`engine::translate::PixelTranslation`) and applies it
inside the projection; the composition root applies the same L2 mapping to
platform input so L3 only ever sees VM coordinates. The three spaces and the
rule that L2 is the only layer that names two of them are in
[`arch/overall.md`](../../arch/overall.md) §3 and
[`rules/arch_rules.md`](../../rules/arch_rules.md) §5.

---

## 6. Cross-cutting invariants

1. **Two engines can run in one process.** Every value is a field of an
   object; there are no globals, statics, `thread_local!`, `LazyLock` or
   process-wide registries.
2. **Errors are values.** Each domain owns one `#[non_exhaustive]` error enum
   (`EngineError`, `TranslateError`, `TextError`, `SoundError`, `VideoError`,
   `TransError`, `TimerError`, `MountError`, `StorageReadError`,
   `StartupError`); there is no `unwrap_or` on a fallible result and no
   wildcard arm that swallows a variant. The rules are in
   [`rules/api_rules.md`](../../rules/api_rules.md) §5.
3. **The projection is a field copy.** Traversal, batching, blend-state
   selection and drawing are L1's.
4. **The script-visible surface is L3's.** L2 names generic concepts
   (`BlendMode`, `Rect`, `BitmapState`); L3 owns the TVP member names, the
   `Layer.type` numbering, the storage naming and the KAG semantics.
5. **Time enters in one place per subsystem.** The engine owns the frame
   clock; the timer registry takes `now_ms` from it; the mixer, the video
   overlay and the transition take a `dt`. Nothing reads a wall clock except
   `std::time::Instant` for the frame report's timing counters.

---

## 7. Tests

`crates/engine/tests/` holds the integration tests, one file per module:

| file | covers |
|---|---|
| `engine.rs` | the frame: clock, projection cadence, upload retry, presentation faults |
| `scene.rs`, `bitmap.rs`, `oop_semantics.rs` | the layer tree, bitmaps, focus, ordering |
| `projection.rs` | the L2 → L1 seam |
| `translate.rs` | the VM ↔ device mapping |
| `layer_ops.rs`, `raster.rs` | the pixel operations |
| `text.rs`, `text_render.rs` | shaping, coverage, atlas, projection |
| `storage.rs` | the namespace and its resolution order |
| `startup.rs` | startup discovery |
| `sound.rs` | the mixer, channels, loops, streaming |
| `video.rs` | the overlay playback machine |
| `trans.rs`, `extrans_*.rs` | the transition machine and each `extrans` provider |
| `shader_library.rs` | the shader-program registry |
| `timer.rs` | the timer registry and its re-entrancy contract |
| `input.rs` | the input state and the frame protocol |

Unit tests live next to the code in `#[cfg(test)] mod tests` blocks
(`scene.rs`, `layer_ops.rs`, `raster.rs`, `sound.rs`, `storage.rs`, `text.rs`,
`video.rs`, `engine.rs`). The behaviour assertions cite `reference/cpp/...:line`;
the test rules are in [`rules/code_styles.md`](../../rules/code_styles.md) §8.

The **main agent** runs the build, clippy and test gate; the compiler-free
checks (`stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `storage-media`, `native-construction`, `lifecycle-order`,
`native-arity`, `void-getter`, `host-facts`, `android-target`,
`surface-lock`, `native-parity`, `plugin-parity`) run without a build and can
be run from a worker.

---

## 8. Evidence and history

The design record behind this crate — the decision log, the middle-layer API,
the layer-tree contract, the presentation rules and the L3 ↔ L2 protocol — is
kept in the archived trail:

| topic | archived document |
|---|---|
| the decisions (D1–D41) and their rationale | [`docs.old/migration/README.md`](../../../docs.old/migration/README.md) §3 |
| the stateless-object / explicit-state API | [`docs.old/migration/03-middle-layer-api.md`](../../../docs.old/migration/03-middle-layer-api.md) |
| the layer tree → projection → frame contract | [`docs.old/migration/12-layer-tree-and-frame.md`](../../../docs.old/migration/12-layer-tree-and-frame.md) |
| the VM ↔ device pixel mapping | [`docs.old/migration/11-presentation-and-pixel-translation.md`](../../../docs.old/migration/11-presentation-and-pixel-translation.md) |
| the L3 ↔ L2 protocol, item by item | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |

The current, authoritative statements of the same material are the
[`arch/`](../../arch/) and [`rules/`](../../rules/) pages.

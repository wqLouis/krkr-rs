//! The engine: the layer that owns every evolving value.
//!
//! The crate owns the scene (windows, layers, bitmaps, fonts), the storage
//! namespace, the mixer, the timer registry, the input state, the text
//! engine, the video overlay and the transition machine, plus the state
//! values the stateless platform services take (`RenderState`, `FileState`,
//! the decoder states). It performs actions on the stateless platform
//! objects and keeps no process-global state: every value is a field of a
//! caller-owned object.
//!
//! The crate depends on the platform type crates (`api`, `fs` and `render`)
//! and on no render or media backend. The storage namespace is built over the
//! platform [`fs::FileSystem`] object; the remaining platform services are
//! reached through the API types ([`api`]).
//!
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod hasher;

#[cfg(feature = "simd-experiment")]
mod simd;

pub mod clock;
pub mod engine;
pub mod imagedecode;
pub mod input;
pub mod layer_ops;
pub mod projection;
pub mod raster;
pub mod scene;
pub mod shader;
pub mod sound;
pub mod startup;
pub mod storage;
pub mod text;
pub mod timer;
pub mod trans;
pub mod translate;
pub mod video;

// The crate root is a curated surface: it re-exports the names a consumer
// reaches through `engine::`, and nothing else. Every other item stays
// reachable through the module that owns it (`engine::layer_ops`,
// `engine::raster`, `engine::projection`, `engine::scene`,
// `engine::shader`, `engine::storage`, `engine::startup`, …), so the flat
// root never becomes an accidental copy of the crate's whole API.

// The frame clock: the time base the frame advances and the script reads,
// and the clock source the caller injects the frame's elapsed time into.
pub use crate::clock::{FrameClock, FrameTimeSource, InjectedFrameTime};
// The frame owner: the clock, the timer pump, the projection and the single
// render call, plus the one upward hook the composition root implements.
pub use crate::engine::{
    Engine, EngineError, EngineOverlay, EngineParts, FrameReport, GameFrame, ObjectCounts,
};
// The fixed-seed lookup map the engine keys its per-bitmap tables with.
pub use crate::hasher::Map;
// The image decode pool the engine owns and drains every frame.
pub use crate::imagedecode::DecodePool;
// The VM-to-device pixel mapping the engine applies when it projects.
pub use crate::translate::{PixelTranslation, TranslateError};
// Archive name normalization is a shared storage-naming rule; the parser
// itself lives in `fs::xp3`.
pub use fs::xp3::normalize_in_archive_name;
// The scene: the logical tree the projection reads.
pub use scene::{
    BitmapState, DecodedImageKey, DecodedImageMode, FontState, LayerState, Rect, Scene,
    TransitionFrame, TransitionSource,
};
// The projection: the scene-to-render-tree conversion and the blend
// resolution it applies.
pub use projection::{
    ComposedLayer, compose_layer, project_scene, project_window, project_window_report,
    resolve_blend,
};
// The storage namespace: the mounts, the resolved locations, the
// storage-media providers and the typed read errors.
pub use storage::{Location, Storage, StorageMedia, StorageReadError};
// The startup-script discovery.
pub use startup::{StartupError, discover_startup};

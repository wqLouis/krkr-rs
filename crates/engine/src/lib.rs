//! krkr-rs middle layer.
//!
//! Owns all state: the TVP layer system, the storage namespace, the mixer,
//! and every bottom-defined state value (`RenderScene`, `RenderState`,
//! `FileState`, decoder states). Performs actions on the stateless bottom
//! objects.
//!
//! This crate must not depend on Bevy, wgpu, `ash`, winit, FFmpeg, cpal or any
//! render/media backend. The storage namespace is the exception: it is built
//! over the bottom [`fs::FileSystem`] object, while the remaining bottom
//! services are reached through the API types ([`api`]).
//!
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod hasher;

pub mod engine;
pub mod input;
pub mod layer_ops;
pub mod projection;
pub mod raster;
pub mod scene;
pub mod sound;
pub mod startup;
pub mod storage;
pub mod text;
pub mod timer;
pub mod trans;
pub mod translate;
pub mod video;

pub use api::config::CacheLimit;

// Archive name normalization is a shared storage-naming rule; the parser
// itself now lives in L1 `fs::xp3`.
pub use fs::xp3::normalize_in_archive_name;
// The fixed-seed lookup map the engine keys its per-bitmap tables with.
pub use crate::engine::{
    Engine, EngineError, EngineOverlay, EngineParts, FrameReport, GameFrame, ObjectCounts,
};
pub use crate::hasher::Map;
pub use crate::translate::{PixelTranslation, TranslateError};
// Curated crate root. The
// pixel primitives, blend-type numbering and TVP constants stay reachable
// through their own modules (`layer_ops`, `raster`, `projection`, `scene`,
// `storage`, `startup`); they are not part of the stable flat surface.
pub use projection::{
    ComposedLayer, ProjectionReport, compose_layer, project_scene, project_scene_report,
    project_window, project_window_report, resolve_blend,
};
pub use scene::{
    BitmapState, DecodedImageKey, DecodedImageMode, FontState, LayerState, Rect, Scene, WindowState,
};
pub use startup::{StartupError, discover_startup};
pub use storage::{Location, MountError, Storage, StorageMetadata, StorageReadError};

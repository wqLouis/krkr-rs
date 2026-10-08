//! krkr-rs middle layer.
//!
//! Owns all state: the TVP layer system, the storage namespace, the mixer,
//! and every bottom-defined state value (`RenderScene`, `RenderState`,
//! `FileState`, decoder states). Performs actions on the stateless bottom
//! objects. See `docs/migration/02-target-architecture.md`.
//!
//! This crate must not depend on Bevy, wgpu, `ash`, winit, FFmpeg, cpal or any
//! render/media backend ([`docs/migration/02-target-architecture.md`] Invariant
//! B). The storage namespace is the exception described in
//! `docs/migration/06-file-system.md`: it is built over the bottom
//! [`fs::FileSystem`] object, while the remaining bottom services are
//! reached through the API types ([`api`]).
//!
//! [`docs/migration/02-target-architecture.md`]: ../../../docs/migration/02-target-architecture.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

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
pub mod video;

// Archive name normalization is a shared storage-naming rule; the parser
// itself now lives in L1 `fs::xp3`.
pub use fs::xp3::normalize_in_archive_name;
// Curated crate root (see `docs/migration/13-l2-api-stability.md` §3). The
// pixel primitives, blend-type numbering and TVP constants stay reachable
// through their own modules (`layer_ops`, `raster`, `projection`, `scene`,
// `storage`, `startup`); they are not part of the stable flat surface.
pub use projection::{ComposedLayer, compose_layer, project_scene, project_window, resolve_blend};
pub use scene::{BitmapState, FontState, LayerState, Rect, Scene, WindowState};
pub use startup::{StartupError, discover_startup};
pub use storage::{Location, MountError, Storage, StorageMetadata, StorageReadError};

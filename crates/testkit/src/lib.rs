//! Dev-only helpers shared by the krkr-rs integration tests.
//!
//! This crate exists so synthetic fixtures and process-global test helpers are
//! not duplicated across integration-test binaries. It is never a runtime
//! dependency; the crates that need it pull it in under `[dev-dependencies]`.
//!
//! The modules are:
//! - [`archives`]: the in-memory XP3 archive builders.
//! - [`game`]: the real-game directory helpers.
//! - [`media_fixtures`]: synthetic image, audio, and TLG byte builders.
//! - [`render`]: a headless window handle for the null render backend.
//! - [`vm`]: a process-global lock serializing TJS VM tests.
//!
//! The crate root re-exports the intended contract item by item, so a test
//! writes `testkit::build_xp3(…)` rather than naming the module.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod archives;
pub mod game;
pub mod media_fixtures;
pub mod render;
pub mod vm;

pub use archives::{build_xp3, build_xp3_compressed_index};
pub use game::game_archives;
pub use media_fixtures::{
    SDS_MAGIC, TLG5_MAGIC, TLG6_MAGIC, encode_png, encode_tlg5, encode_tlg6, pattern, test_image,
    tiny_wav_mono,
};
pub use render::headless_window;
pub use vm::vm_lock;

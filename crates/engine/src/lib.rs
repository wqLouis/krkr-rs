//! krkr-rs engine — the **load module**.
//!
//! Goal of this milestone: load a KiriKiri game — mount its storage (game
//! directory + `.xp3` archives), discover `startup.tjs`, bootstrap the C++
//! TJS2 VM and run the script. This crate does no rendering or windowing;
//! the Bevy app lives in `crates/render`.
//!
//! Storage naming follows the reference engine (`TVPSearchPlacedPath`):
//! names are normalized (lowercase, `/` separators), disk files win over
//! archives, and `arc.xp3>path` addresses a file inside a specific archive.

pub mod loader;
pub mod storage;

pub use loader::{LoadReport, init_logging, load_game};
pub use storage::Storage;

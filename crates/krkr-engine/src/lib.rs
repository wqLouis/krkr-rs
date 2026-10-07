//! krkr-rs middle layer.
//!
//! Owns all state: the TVP layer system, the storage namespace, the mixer,
//! and every bottom-defined state value (`RenderScene`, `RenderState`,
//! `FileState`, decoder states). Performs actions on the stateless bottom
//! objects. See `docs/migration/02-target-architecture.md`.

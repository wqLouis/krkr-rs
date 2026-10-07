//! krkr-rs bottom file system.
//!
//! Stateless `FileSystem` object + explicit `FileState` (handle + cursor).
//! The middle layer owns all `FileState`s and the storage namespace. See
//! `docs/migration/06-file-system.md`.

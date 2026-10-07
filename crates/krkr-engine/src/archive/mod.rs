//! Archive containers read over `krkr_fs` positional IO (L2).
//!
//! Archives are parsed by the middle layer from a `FileState` supplied by the
//! bottom `FileSystem`; the bottom layer knows nothing about `.xp3`. See
//! `docs/migration/06-file-system.md` §2 and §3.

pub mod xp3;

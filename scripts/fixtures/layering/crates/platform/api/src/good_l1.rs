//! Near miss: the words Layer, TVP and KAG in prose, and extensions inside
//! string literals, are not vocabulary in code.  `Storage` is this crate's own
//! type, not the TVP storage namespace.
//!
//! The scan must ignore all of it.

pub struct Storage;

/// The engine's `Layer.type` is documented here, in a comment only.
pub fn layer_name() -> &'static str {
    "Layer.sli"
}

/// A script path is a literal, not a token.
pub fn script_path() -> &'static str {
    "scripts/main.tjs"
}

pub fn with_note(p: *const u8) -> u8 {
    // SAFETY: the caller guarantees `p` is valid for one byte.
    unsafe { *p }
}

//! Shared MD5 helper for the plugins that expose the reference's
//! `getMD5HashString` surface.
//!
//! The reference computes RFC 1321 MD5 through `TVP_md5_*`; this helper
//! produces the same lowercase 32-character hex form through the `md-5`
//! crate. Both `Storages.getMD5HashString` and `Scripts.getMD5HashString`
//! call it.

use md5::{Digest, Md5};

/// Lowercase 32-character hex MD5 of `input` (reference `TVP_md5_*` output).
pub(crate) fn md5_hex(input: &[u8]) -> String {
    // The RustCrypto digest output renders as lowercase hex; this is the
    // only formatting the reference ever produces.
    format!("{:x}", Md5::digest(input))
}

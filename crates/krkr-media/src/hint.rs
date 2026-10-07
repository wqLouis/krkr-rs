//! Shared parsing helpers for storage-name/extension hints.
//!
//! The image and audio routers both accept a "hint" — a full storage name
//! such as `"frm/bg01.tlg5"` or a bare extension such as `"tlg6"` / `".wav"`.
//! This module holds the single copy of the extraction logic so the two
//! routers cannot drift.

use std::path::Path;

/// Extracts the extension from a filename or bare-extension hint.
///
/// Accepts a full storage name (`"dir/a.png"`), a bare extension (`"png"`),
/// or a leading-dot extension (`".png"`). The slice is returned as-is: no
/// allocation and no case folding, so callers compare case-insensitively
/// (`eq_ignore_ascii_case`) or hand it to APIs that already do.
pub(crate) fn extension_of(hint: &str) -> Option<&str> {
    if let Some(ext) = Path::new(hint).extension().and_then(|ext| ext.to_str()) {
        return Some(ext);
    }
    let bare = hint.strip_prefix('.').unwrap_or(hint);
    if !bare.is_empty() && !bare.contains(['/', '\\', '.']) {
        return Some(bare);
    }
    None
}

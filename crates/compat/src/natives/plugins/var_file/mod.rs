//! `varfile.dll` — the `var://` script-variable storage media.
//!
//! Reference: `reference/cpp/plugins/varfile.cpp`. The plugin's entire
//! surface is one `iTVPStorageMedia` registered through
//! `TVPRegisterStorageMedia`: a `var://` scheme whose "files" are TJS
//! dictionary members (`Octet` values). It registers **no** `Storages`
//! members — the file-manipulation members the core `Storages` class exposes
//! come from `fstat/main.cpp` and are owned by [`super::fstat`].
//!
//! # Missing seam
//!
//! The L2 storage namespace (`engine_api::Storage`) has no storage-media
//! registration seam. A `Storage` is built from an immutable L1 `FileSystem`
//! plus explicitly mounted XP3 archives, and its media are fixed to the
//! disk/archive backends; there is no `TVPRegisterStorageMedia` equivalent
//! (reference: `varfile.cpp:371-377` registers the media from the
//! pre-registration callback, and `varfile.cpp:189-368` implements the
//! `iTVPStorageMedia` vtable over script dictionary members —
//! `CheckExistentStorage` at `:237`, `Open` at `:244`, `GetListAt` at
//! `:263`, all keyed by the `var://` domain walked in `getParentName`
//! `:285-337`). A `var://` media cannot be plugged in without an L2
//! extension point — for example a
//! `Storage::register_media(name, Box<dyn StorageMedia>)` whose
//! `CheckExistentStorage` / `Open` / `GetListAt` / `GetLocallyAccessibleName`
//! operations the compat layer could implement over a TJS dictionary.
//!
//! Per the porting rules this module does **not** fabricate a fake `var://`
//! handler or an over-scoped `Storages` member. [`register`] is a documented
//! no-op and the missing seam is reported here.

use crate::{CompatCtx, CompatError};

/// No `var://` media registration seam exists in L2.
///
/// The reference only registers a storage media (`PreRegistCallback` →
/// `TVPRegisterStorageMedia(var)`); `engine_api::Storage` exposes no media hook,
/// so there is nothing to register. See the module documentation for the
/// precise missing seam.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}

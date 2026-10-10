//! `dirlist.dll` — the global `getDirList(dir)` function.
//!
//! Reference: `reference/cpp/plugins/dirlist.cpp`. The plugin registers a
//! single **global** function through its post-registration callback,
//! `getDirList(dir)`, which returns an array of the immediate child names of
//! `dir` (the `TVPGetLocalFileListAt` listing). It registers **no** `Storages`
//! members — the `Storages.dirlist` / `dirlistEx` / `dirtree` /
//! `createDirectory` / ... surface comes from `fstat/main.cpp` and is owned by
//! [`super::fstat`].
//!
//! # Implementation note
//!
//! The `tjs` engine has no native global-function registration seam (the
//! analogue of the parity parser's `NCB_REGISTER_FUNCTION`); every plugin that
//! installs a global function does so with a small TJS wrapper. Following the
//! reference's semantics, the wrapper delegates to the fstat-owned
//! `Storages.dirlist` member, which enforces the reference's trailing-`/`
//! rule (`reference/cpp/plugins/dirlist.cpp:25-27`,
//! `reference/cpp/plugins/fstat/main.cpp:509-512`) and returns the immediate
//! child names as an array (`reference/cpp/plugins/fstat/main.cpp:525-531`).
//! The wrapper then lowercases every name: the reference's `getDirList` reads
//! through `TVPGetLocalFileListAt` (`reference/cpp/plugins/dirlist.cpp:41-45`),
//! which lowercases each entry before the callback sees it
//! (`reference/cpp/core/base/impl/StorageImpl.cpp:234-241`), while the fstat
//! member does not (`fstat/main.cpp:526`).
//!
//! Two disclosed differences remain. The reference's `getDirList` and
//! `Storages.dirlist` are two *independent* C++ implementations
//! (`dirlist.cpp:13-57` and `fstat/main.cpp:506-551`), so a script that
//! replaces one name does not affect the other; the port routes both names
//! through the single native member, so reassigning `Storages.dirlist`
//! changes what `getDirList` delegates to. `dirlist.cpp` also filters the
//! listing to `S_IFREG | S_IFDIR` (`dirlist.cpp:42`) while the fstat member
//! keeps every entry; the delegated member exposes no file-type field, so the
//! port keeps every entry. A directory that cannot be listed raises the fstat
//! member's typed `"Directory not found."` error (`fstat/main.cpp:520-521`),
//! where the reference `getDirList` returns an empty array
//! (`StorageImpl.cpp:222-247` does nothing when `opendir` fails, and the
//! `catch` at `dirlist.cpp:48-50` only rethrows other exceptions).
#![allow(unsafe_code)]

use crate::{CompatCtx, CompatError};

/// The reference `tGetDirListFunction`: the immediate child names of `dir`,
/// lowercased as `TVPGetLocalFileListAt` lowercases them.
const GET_DIR_LIST_SCRIPT: &str = "\
function getDirList(dir) {
    var list = Storages.dirlist(dir);
    for (var i = 0; i < list.length; ++i) {
        list[i] = list[i].toLowerCase();
    }
    return list;
}";

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: `ctx` is only created for a bound, live engine.
    unsafe { ctx.engine() }
        .exec_script(GET_DIR_LIST_SCRIPT, "dirlist")
        .map_err(|error| {
            CompatError::Register(format!("dirlist: cannot install getDirList: {error}"))
        })?;
    Ok(())
}

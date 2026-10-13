//! `Storages` native class (L3) over the L2 [`engine::Storage`] namespace.
//!
//! This is a reimplementation on the middle-layer API. The mounted namespace
//! is the one the
//! host attached through [`CompatServices::storage`]; all archive/namespace
//! policy (`arc.xp3>path`, case folding, the auto search paths) lives in
//! [`engine::Storage`] and is never duplicated here.
//!
//! Script-visible members (reference `tTJSNC_Storages`):
//!
//! | member | behavior |
//! |---|---|
//! | `addAutoPath(path)` / `removeAutoPath(path)` | maintain the L2 auto-path list; the path must end with `/`, `\` or `>` |
//! | `getFullPath(path)` | normalized storage name (lowercase, `/` separators, in-archive part additionally collapsed) |
//! | `getPlacedPath(path)` | normalized placed name when `Storage::find` resolves it, `""` otherwise; positive results are cached in per-family dispatch state |
//! | `isExistentStorage(name)` | whether the name resolves to a file or directory in the mounted storage/auto paths |
//! | `extractStorageExt/Name/Path`, `chopStorageExt` | pure string helpers over `/`, `\` and the `>` archive delimiter |
//! | `clearArchiveCache()` | clears the L3 placed-path cache (the L2 mounts are parsed eagerly) |
//! | `getLocalName(name)` | absolute disk path for a disk file; raises for archive-only names; name unchanged otherwise |
//!
//! Per-family state (the placed-path cache and the string/array output
//! scratch buffers) lives in [`CompatServices::state`], so the only
//! process-wide state remains the documented engine → context binding.
//!
//! # Host-facts seam
//!
//! `searchCD` and `selectFile` match the reference's inert values: the
//! reference `TVPSearchCD` returns the empty string and `TVPSelectFile`
//! returns `false` when no file chooser is available (`StorageImpl.cpp:1232-1246`,
//! `FileSelector.cpp:100-420`). Neither composes a host seam.
//!
//! Deferred: `open` (the binary-stream object) and the `copyFile` /
//! `deleteFile` / `selectDirectory` members of the reference. They need
//! host/GUI facilities that do not exist in the L2 API yet; they are simply
//! not registered, so a script calling them gets the ordinary TJS "member does
//! not exist" error rather than a silent fallback.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use engine::{Location, Storage, StorageReadError, normalize_in_archive_name};
use tjs::{Engine, NativeClassBuilder, NativeMethodDef, VAL_OBJECT, VAL_STRING, Value};

use crate::binding;
use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::{CompatCtx, CompatError, CompatServices};

/// Validates and normalizes a `Storages.addAutoPath`/`removeAutoPath`
/// argument, enforcing the reference's trailing-delimiter rule.
fn validate_auto_path(method: &str, path: &str) -> Result<String, String> {
    if !path.ends_with(['/', '\\', '>']) {
        return Err(format!(
            "Storages.{method}: path must end with '/', '\\\\' or '>'; got \"{path}\""
        ));
    }
    let entry = normalize_storage_name(path);
    if entry.trim_end_matches(['/', '>']).is_empty() {
        return Err(format!("Storages.{method}: empty path"));
    }
    Ok(entry)
}

/// `TVPNormalizeStorageName`: unify path delimiters and lowercase the
/// disk/domain path (`tTVPFileMedia::NormalizeDomainName` /
/// `NormalizePathName`, `reference/cpp/core/base/impl/StorageImpl.cpp:95-117`,
/// applied by `TVPNormalizeStorageName` at
/// `reference/cpp/core/base/StorageIntf.cpp:362-369`). The in-archive part
/// after `>` is additionally normalized the way
/// `tTVPArchive::NormalizeInArchiveStorageName` does — lowercase, `\` → `/`,
/// collapse runs of `/`, drop leading slashes
/// (`reference/cpp/core/base/StorageIntf.cpp:566-606`). Shared with the
/// `Debug.logLocation` setter, which applies the same normalization
/// (`reference/cpp/core/utils/DebugIntf.cpp:518-520`).
pub(crate) fn normalize_storage_name(name: &str) -> String {
    match name.split_once('>') {
        Some((disk, in_archive)) => format!(
            "{}>{}",
            disk.replace('\\', "/").to_ascii_lowercase(),
            normalize_in_archive_name(in_archive)
        ),
        None => name.replace('\\', "/").to_ascii_lowercase(),
    }
}

/// `TVPExtractStorageExt`: the extension including the dot, or `""`.
fn extract_storage_ext(name: &str) -> String {
    for (i, c) in name.char_indices().rev() {
        match c {
            '\\' | '/' | '>' => return String::new(),
            '.' => return name[i..].to_string(),
            _ => {}
        }
    }
    String::new()
}

/// `TVPExtractStorageName`: the final path component.
pub(crate) fn extract_storage_name(name: &str) -> String {
    match name.rfind(['\\', '/', '>']) {
        Some(i) => name[i + 1..].to_string(),
        None => name.to_string(),
    }
}

/// `TVPExtractStoragePath`: the path part including the last delimiter.
fn extract_storage_path(name: &str) -> String {
    match name.rfind(['\\', '/', '>']) {
        Some(i) => name[..=i].to_string(),
        None => String::new(),
    }
}

/// `TVPChopStorageExt`: the name without its extension (dot removed).
fn chop_storage_ext(name: &str) -> String {
    for (i, c) in name.char_indices().rev() {
        match c {
            '\\' | '/' | '>' => return name.to_string(),
            '.' => return name[..i].to_string(),
            _ => {}
        }
    }
    name.to_string()
}

/// Positive-result cache for `getPlacedPath` (the reference's
/// `TVPAutoPathCache`), keyed by the requested name.
#[derive(Default)]
struct PlacedCache(HashMap<String, String>);

/// Scratch buffer for a string result; the C++ trampoline copies it before the
/// next native call.
#[derive(Default)]
struct StringScratch(Vec<u8>);

/// Write a string result into `out` using the shared scratch buffer.
fn set_string_out(out: *mut Value, services: &CompatServices, s: &str) {
    let mut scratch = services.state::<StringScratch>();
    scratch.0.clear();
    scratch.0.extend_from_slice(s.as_bytes());
    scratch.0.push(0);
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = scratch.0.as_ptr() as *const c_char;
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = 0;
        }
    }
}

/// The attached storage pointer, if the host attached one.
fn attached_storage(services: &CompatServices) -> Option<*mut Storage> {
    services.storage()
}

/// Formats a storage error for a `Storages` member.
///
/// A message that originates in the reference is raised verbatim:
/// `TVPInvalidPathName` is `"Invalid path name %1"`
/// (`reference/cpp/core/base/MsgIntfInc.h:28`) and is thrown from
/// `TVPNormalizeStorageName` when a `..` escapes the mount
/// (`reference/cpp/core/base/StorageIntf.cpp:400`). A message that is ours has
/// no reference counterpart, so it keeps the `Storages.<member>: ` prefix that
/// says which member failed.
fn storage_error_message(member: &str, error: &StorageReadError) -> String {
    match error {
        StorageReadError::InvalidPathName(_) => error.to_string(),
        _ => format!("Storages.{member}: {error}"),
    }
}

/// Compute the placed path for `name` through the L2 [`Storage::find`].
///
/// Documented approximation of `TVPGetPlacedPath`
/// (`reference/cpp/core/base/StorageIntf.cpp:1061-1097`): the reference returns
/// the normalized absolute path it resolved (`CurrentPath` plus the request, or
/// the absolute auto-path prefix plus the base name), while this returns the
/// game-directory-relative disk path and `archive-file-name>in-archive-path`.
/// Both still resolve back to the same storage through `find`/`read`.
fn compute_placed_path(storage: &Storage, name: &str) -> Result<String, String> {
    match storage.find(name) {
        Ok(Some(Location::Disk(path))) => {
            let relative = path
                .strip_prefix(storage.game_dir())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| normalize_storage_name(name));
            Ok(normalize_storage_name(&relative))
        }
        Ok(Some(Location::Archive(archive, in_archive))) => {
            let archive = archive
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(format!("{archive}>{in_archive}"))
        }
        Ok(None) => Ok(String::new()),
        Err(error) => Err(storage_error_message("getPlacedPath", &error)),
    }
}

/// Resolve the process-wide binding for a callback's raw engine pointer.
///
/// Returns the resolved binding; callers derive the context and services from
/// it. The macro writes a `not bound` error and returns from the callback when
/// resolution fails.
macro_rules! resolve_or_return {
    ($engine:expr, $out_error:expr, $method:literal) => {
        match binding::resolve($engine as *mut Engine) {
            Some(resolved) => resolved,
            None => {
                return report_error(
                    $out_error,
                    concat!("Storages.", $method, ": compat context is not bound"),
                );
            }
        }
    };
}

// ---------------------------------------------------------------------------
// FFI glue
// ---------------------------------------------------------------------------

extern "C" fn native_add_auto_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "addAutoPath");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(path) = args.first() else {
        return report_error(out_error, "Storages.addAutoPath requires 1 argument");
    };
    let path = value_as_string(path);
    let entry = match validate_auto_path("addAutoPath", &path) {
        Ok(entry) => entry,
        Err(message) => return report_error(out_error, &message),
    };
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.addAutoPath: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime,
    // and the VM is single-threaded, so no other reference is live here.
    unsafe { &mut *storage }.add_auto_path(entry);
    // A new search path invalidates the positive placed-path cache.
    services.state::<PlacedCache>().0.clear();
    set_void(out);
    0
}

extern "C" fn native_remove_auto_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "removeAutoPath");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(path) = args.first() else {
        return report_error(out_error, "Storages.removeAutoPath requires 1 argument");
    };
    let path = value_as_string(path);
    let entry = match validate_auto_path("removeAutoPath", &path) {
        Ok(entry) => entry,
        Err(message) => return report_error(out_error, &message),
    };
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.removeAutoPath: no storage attached");
    };
    // SAFETY: see `native_add_auto_path`.
    unsafe { &mut *storage }.remove_auto_path(&entry);
    services.state::<PlacedCache>().0.clear();
    set_void(out);
    0
}

extern "C" fn native_get_full_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getFullPath");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.getFullPath requires 1 argument");
    };
    set_string_out(
        out,
        services,
        &normalize_storage_name(&value_as_string(name)),
    );
    0
}

extern "C" fn native_get_placed_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getPlacedPath");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.getPlacedPath requires 1 argument");
    };
    let name = value_as_string(name);
    if name.is_empty() {
        set_string_out(out, services, "");
        return 0;
    }
    // The temporary cache borrow must end before the string scratch is
    // borrowed (the state map is one `RefCell`), so copy the hit out first.
    let cached = services.state::<PlacedCache>().0.get(&name).cloned();
    if let Some(hit) = cached {
        set_string_out(out, services, &hit);
        return 0;
    }
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.getPlacedPath: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let placed = match compute_placed_path(unsafe { &*storage }, &name) {
        Ok(placed) => placed,
        Err(message) => return report_error(out_error, &message),
    };
    if !placed.is_empty() {
        services
            .state::<PlacedCache>()
            .0
            .insert(name, placed.clone());
    }
    set_string_out(out, services, &placed);
    0
}

extern "C" fn native_is_existent_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "isExistentStorage");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.isExistentStorage requires 1 argument");
    };
    let name = value_as_string(name);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.isExistentStorage: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    match unsafe { &*storage }.exists(&name) {
        Ok(exists) => {
            set_bool(out, exists);
            0
        }
        Err(error) => report_error(
            out_error,
            &storage_error_message("isExistentStorage", &error),
        ),
    }
}

extern "C" fn native_extract_storage_ext(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "extractStorageExt");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.extractStorageExt requires 1 argument");
    };
    set_string_out(out, services, &extract_storage_ext(&value_as_string(name)));
    0
}

extern "C" fn native_extract_storage_name(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "extractStorageName");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.extractStorageName requires 1 argument");
    };
    set_string_out(out, services, &extract_storage_name(&value_as_string(name)));
    0
}

extern "C" fn native_extract_storage_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "extractStoragePath");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.extractStoragePath requires 1 argument");
    };
    set_string_out(out, services, &extract_storage_path(&value_as_string(name)));
    0
}

extern "C" fn native_chop_storage_ext(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "chopStorageExt");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.chopStorageExt requires 1 argument");
    };
    set_string_out(out, services, &chop_storage_ext(&value_as_string(name)));
    0
}

extern "C" fn native_clear_archive_cache(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "clearArchiveCache");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // The reference clears both the archive cache and the auto-path cache in
    // `TVPClearStorageCaches`; drop the L2 resolver tables as well as the L3
    // placed-path cache (the reference's `TVPAutoPathCache`).
    services.state::<PlacedCache>().0.clear();
    if let Some(storage) = attached_storage(services) {
        // SAFETY: the host keeps the attached storage alive for the VM lifetime.
        unsafe { &mut *storage }.clear_resolution_cache();
    }
    log::debug!("Storages.clearArchiveCache: cleared the placed-path cache");
    set_void(out);
    0
}

extern "C" fn native_get_local_name(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getLocalName");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.getLocalName requires 1 argument");
    };
    let name = value_as_string(name);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.getLocalName: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    match storage.find_disk(&name) {
        Ok(Some(path)) => {
            set_string_out(out, services, &path.to_string_lossy());
            0
        }
        Ok(None) => match storage.find(&name) {
            // The reference throws `TVPCannotGetLocalName` for archive-only
            // names; a plain miss returns the name unchanged.
            Ok(Some(Location::Archive(..))) => report_error(
                out_error,
                &format!(
                    "Storages.getLocalName: \"{name}\" is inside an archive and has no local name"
                ),
            ),
            Ok(Some(Location::Disk(..))) | Ok(None) => {
                set_string_out(out, services, &name);
                0
            }
            Err(error) => report_error(out_error, &storage_error_message("getLocalName", &error)),
        },
        Err(error) => report_error(out_error, &storage_error_message("getLocalName", &error)),
    }
}

/// `Storages.searchCD(label)` → string
///
/// The reference `TVPSearchCD` always returns the empty string because the
/// drive scan is disabled (`StorageImpl.cpp:1232-1246`). The port reproduces
/// that inert value rather than raising.
extern "C" fn native_search_cd(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Storages.searchCD requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let _label = value_as_string(&unsafe { args(argv, argc) }[0]);
    let resolved = resolve_or_return!(engine, out_error, "searchCD");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    log::warn!("Storages.searchCD: no disc search is available; returning an empty string");
    set_string_out(out, services, "");
    0
}

/// `Storages.selectFile(params)` → bool
///
/// The reference `TVPSelectFile` returns `false` when no file chooser can be
/// shown (`FileSelector.cpp:100-420`); the port has no picker, so it returns
/// that value rather than raising. The parameter object is still required
/// (`StorageImpl.cpp:1299-1309`).
extern "C" fn native_select_file(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Storages.selectFile requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a[0].ty != VAL_OBJECT {
        return report_error(
            out_error,
            "Storages.selectFile: the parameter object is required",
        );
    }
    log::warn!("Storages.selectFile: no file picker is available; returning false");
    set_bool(out, false);
    0
}

/// Register the `Storages` native class (static members only).
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Storages",
        methods: vec![
            NativeMethodDef {
                name: "addAutoPath",
                f: native_add_auto_path,
            },
            NativeMethodDef {
                name: "removeAutoPath",
                f: native_remove_auto_path,
            },
            NativeMethodDef {
                name: "getFullPath",
                f: native_get_full_path,
            },
            NativeMethodDef {
                name: "getPlacedPath",
                f: native_get_placed_path,
            },
            NativeMethodDef {
                name: "isExistentStorage",
                f: native_is_existent_storage,
            },
            NativeMethodDef {
                name: "extractStorageExt",
                f: native_extract_storage_ext,
            },
            NativeMethodDef {
                name: "extractStorageName",
                f: native_extract_storage_name,
            },
            NativeMethodDef {
                name: "extractStoragePath",
                f: native_extract_storage_path,
            },
            NativeMethodDef {
                name: "chopStorageExt",
                f: native_chop_storage_ext,
            },
            NativeMethodDef {
                name: "clearArchiveCache",
                f: native_clear_archive_cache,
            },
            NativeMethodDef {
                name: "getLocalName",
                f: native_get_local_name,
            },
            NativeMethodDef {
                name: "searchCD",
                f: native_search_cd,
            },
            NativeMethodDef {
                name: "selectFile",
                f: native_select_file,
            },
        ],
        properties: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference-originated error is raised verbatim; our own diagnostics
    /// keep the member prefix.
    ///
    /// Reference: `TVPInvalidPathName` is `"Invalid path name %1"`
    /// (`reference/cpp/core/base/MsgIntfInc.h:28`), thrown from
    /// `TVPNormalizeStorageName` when a `..` escapes the mount
    /// (`reference/cpp/core/base/StorageIntf.cpp:400`).
    #[test]
    fn invalid_path_name_raises_the_reference_string_verbatim() {
        let error = StorageReadError::InvalidPathName("../../x".to_string());
        assert_eq!(
            storage_error_message("isExistentStorage", &error),
            "Invalid path name ../../x"
        );

        let error = StorageReadError::NotFound("x".to_string());
        assert_eq!(
            storage_error_message("isExistentStorage", &error),
            "Storages.isExistentStorage: storage not found: x"
        );
    }
}

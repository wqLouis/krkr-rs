//! `Storages` native class (L3) over the L2 [`engine::Storage`] namespace.
//!
//! This is a reimplementation on the middle-layer API, not a port of the old
//! `tvp-storages` process-global crate. The mounted namespace is the one the
//! host attached through [`CompatServices::storage`]; all archive/namespace
//! policy (`arc.xp3>path`, case folding, the auto search paths) lives in
//! [`engine::Storage`] and is never duplicated here.
//!
//! Script-visible members (reference `tTJSNC_Storages`):
//!
//! | member | behavior |
//! |---|---|
//! | `addAutoPath(path)` / `removeAutoPath(path)` | maintain the L2 auto-path list; the path must end with `/`, `\` or `>` |
//! | `getFullPath(path)` | normalized storage name (lowercase, `/` separators) |
//! | `getPlacedPath(path)` | normalized placed name when `Storage::find` resolves it, `""` otherwise; positive results are cached in per-family dispatch state |
//! | `isExistentStorage(name)` | whether the name resolves to a file or directory in the mounted storage/auto paths |
//! | `extractStorageExt/Name/Path`, `chopStorageExt` | pure string helpers over `/`, `\` and the `>` archive delimiter |
//! | `clearArchiveCache()` | clears the L3 placed-path cache (the L2 mounts are parsed eagerly) |
//! | `getFileList(mask[, attr])` | `Storage::get_file_list` result as a TJS `Array` |
//! | `getLocalName(name)` | absolute disk path for a disk file; raises for archive-only names; name unchanged otherwise |
//! | `stat(name)` / `fstat(name)` | a TJS dictionary with size and Date timestamps |
//!
//! Per-family state (the placed-path cache and the string/array output
//! scratch buffers) lives in [`CompatServices::state`], so the only
//! process-wide state remains the documented engine → context binding.
//!
//! Deferred: `open` (the binary-stream object) and the `copyFile` /
//! `deleteFile` / `selectFile` / `selectDirectory` / `searchCD` members of the
//! reference. They need host/GUI facilities that do not exist in the L2 API
//! yet; they are simply not registered, so a script calling them gets the
//! ordinary TJS "member does not exist" error rather than a silent fallback.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::{Location, Storage, StorageMetadata};
use tjs::{
    Engine, NativeClassBuilder, NativeMethodDef, RetainedValue, VAL_ARRAY, VAL_INTEGER,
    VAL_RETAINED, VAL_STRING, Value,
};

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

/// Normalize a storage name the way `TVPNormalizeStorageName` does for the
/// common case: ASCII-lowercase and `\` → `/` (media prefixes are out of
/// scope).
fn normalize_storage_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '\\' {
                '/'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
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
fn extract_storage_name(name: &str) -> String {
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

/// Scratch buffers for a `VAL_ARRAY` result; the pointers must outlive the
/// call, so the buffers live in the per-family dispatch state.
#[derive(Default)]
struct ArrayScratch {
    ptrs: Vec<*const c_char>,
    bytes: Vec<Vec<u8>>,
}

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

/// Write a `VAL_ARRAY` result into `out` using the shared scratch buffers.
fn set_array_out(out: *mut Value, services: &CompatServices, values: &[String]) {
    let mut scratch = services.state::<ArrayScratch>();
    scratch.bytes.clear();
    scratch.ptrs.clear();
    for value in values {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        scratch.bytes.push(bytes);
    }
    let mut ptrs = Vec::with_capacity(scratch.bytes.len());
    for bytes in &scratch.bytes {
        ptrs.push(bytes.as_ptr() as *const c_char);
    }
    scratch.ptrs = ptrs;
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_ARRAY;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = scratch.ptrs.as_ptr();
            (*out).array_count = scratch.ptrs.len() as c_int;
            (*out).retained = 0;
        }
    }
}

/// Write a retained id into `out` (`VAL_RETAINED`); the C++ side consumes the
/// retention while converting the native result.
fn set_retained_out(out: *mut Value, id: usize) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = id;
    }
}

/// The attached storage pointer, if the host attached one.
fn attached_storage(services: &CompatServices) -> Option<*mut Storage> {
    services.storage()
}

/// Compute the placed path for `name` through the L2 [`Storage::find`].
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
        // `Location` is `#[non_exhaustive]`; future variants have no placed path.
        Ok(Some(_)) => Ok(String::new()),
        Err(error) => Err(format!("Storages.getPlacedPath: {error}")),
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
        Err(error) => report_error(out_error, &format!("Storages.isExistentStorage: {error}")),
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
    // The L2 mounts parse their indexes eagerly and hold them for the storage
    // lifetime, so there is no archive cache to drop; the only cache is the L3
    // placed-path cache (the reference's `TVPAutoPathCache`).
    services.state::<PlacedCache>().0.clear();
    log::debug!("Storages.clearArchiveCache: cleared the placed-path cache");
    set_void(out);
    0
}

extern "C" fn native_get_file_list(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getFileList");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(mask) = args.first() else {
        return report_error(out_error, "Storages.getFileList requires a mask");
    };
    let mask = value_as_string(mask);
    let attr = args
        .get(1)
        .and_then(|value| (value.ty == VAL_INTEGER).then_some(value.integer))
        .unwrap_or(0);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.getFileList: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    match unsafe { &*storage }.get_file_list(&mask, attr) {
        Ok(names) => {
            set_array_out(out, services, &names);
            0
        }
        Err(error) => report_error(out_error, &format!("Storages.getFileList: {error}")),
    }
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
            Ok(_) => {
                set_string_out(out, services, &name);
                0
            }
            Err(error) => report_error(out_error, &format!("Storages.getLocalName: {error}")),
        },
        Err(error) => report_error(out_error, &format!("Storages.getLocalName: {error}")),
    }
}

/// Convert a host timestamp to the seconds representation the reference uses.
fn unix_seconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs())
            .ok()
            .map(|seconds| -seconds),
    }
}

/// Build the TJS dictionary expression the `stat`/`fstat` natives evaluate.
fn stat_dictionary_script(metadata: &StorageMetadata) -> String {
    let mut script = format!(
        "(function(){{ var d = %[]; d[\"size\"] = {};",
        metadata.size
    );
    for (name, value) in [
        ("mtime", metadata.modified),
        ("ctime", metadata.created),
        ("atime", metadata.accessed),
    ] {
        if let Some(seconds) = value.and_then(unix_seconds) {
            script.push_str(&format!(" d[\"{name}\"] = new Date({seconds});"));
        }
    }
    script.push_str(" return d; })()");
    script
}

extern "C" fn native_stat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "stat");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let args = unsafe { args(argv, argc) };
    let Some(name) = args.first() else {
        return report_error(out_error, "Storages.stat requires 1 argument");
    };
    let name = value_as_string(name);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Storages.stat: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let metadata = match unsafe { &*storage }.stat(&name) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => {
            return report_error(
                out_error,
                &format!("Storages.stat: storage not found: {name}"),
            );
        }
        Err(error) => return report_error(out_error, &format!("Storages.stat: {error}")),
    };
    let expression = stat_dictionary_script(&metadata);
    match resolved
        .engine()
        .eval_retained(&expression, "Storages.stat")
    {
        Ok(RetainedValue::Object(object)) => {
            set_retained_out(out, object.raw_id() as usize);
            // The C++ trampoline consumes the retention when it converts the
            // native result, so the guard must not release it first.
            std::mem::forget(object);
            0
        }
        Ok(RetainedValue::Value(_)) => {
            report_error(out_error, "Storages.stat: unexpected scalar result")
        }
        Err(error) => report_error(out_error, &format!("Storages.stat: {error}")),
    }
}

extern "C" fn native_fstat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    native_stat(engine, argc, argv, out, out_error)
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
                name: "getFileList",
                f: native_get_file_list,
            },
            NativeMethodDef {
                name: "getLocalName",
                f: native_get_local_name,
            },
            NativeMethodDef {
                name: "stat",
                f: native_stat,
            },
            NativeMethodDef {
                name: "fstat",
                f: native_fstat,
            },
        ],
        properties: vec![],
    })
}

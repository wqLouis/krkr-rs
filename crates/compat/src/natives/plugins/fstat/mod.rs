//! `fstat.dll` plugin — a full port of `reference/cpp/plugins/fstat/main.cpp`.
//!
//! The reference does two things:
//!
//! * `NCB_ATTACH_CLASS(StoragesFstat, Storages)` extends the core `Storages`
//!   class with file-statistics, directory, copy/move/delete and path helpers;
//! * `NCB_REGISTER_CLASS(TemporaryFiles)` registers a small instance class
//!   whose `entry` / `entryFolder` members validate a temporary file/folder.
//!
//! Every member of the reference attach list is registered here. The Rust
//! port attaches the `Storages` members as **static** members (the same shape
//! the sibling [`crate::natives::storages`] port uses), so scripts call them
//! exactly like the reference: `Storages.fstat(...)`.
//!
//! # Layers
//!
//! All storage namespace policy (normalization, `arc.xp3>path`, case folding,
//! auto paths) lives in the L2 [`engine::Storage`] attached to
//! [`CompatServices`]; this plugin never reimplements it. Names that resolve
//! to a disk file use the real on-disk path; names that resolve inside an
//! archive are reported by `fstat` with their uncompressed size only, exactly
//! like the reference.
//!
//! # Timestamps and `Date`
//!
//! The reference stores file times in TJS `Date` objects by calling
//! `Date.setTime(filetime)` with the raw `tTVP_stat` seconds
//! (`reference/cpp/plugins/fstat/main.cpp:112-133`) and reads them back by
//! handing `Date.getTime()` straight to `TVP_utime`
//! (`main.cpp:135-148,215-218`). `Date.setTime` divides by 1000 and
//! `Date.getTime` multiplies by 1000
//! (`reference/cpp/core/tjs2/tjsDate.cpp:209-219,300-306`), so the
//! observable `Date.getTime()` value equals the file time the reference
//! round-trips — raw epoch seconds truncated to a multiple of 1000 — while
//! the `Date` *displays* as epoch + filetime/1000. This port reproduces the
//! shipped behaviour verbatim: no unit conversion at either boundary.
//!
//! # Reference no-ops
//!
//! A handful of the reference members are explicitly unimplemented on
//! non-Windows platforms and only log a warning: `truncateFile`,
//! `changeDirectory`, `setFileAttributes`, `resetFileAttributes`,
//! `getFileAttributes` and `selectDirectory`. They keep the reference's
//! documented return values (a warning plus `true`, or `0` for the getter);
//! they are not silent stubs and are called out here.
//!
//! # Instance state
//!
//! `TemporaryFiles` follows the reference's implementation (which validates
//! but does not record entries): its native payload is a unit value. The
//! reference's `~TemporaryFiles` has nothing to release either.

use super::md5::md5_hex;

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::{Location, Storage};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeMethodDef,
    NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue,
    VAL_OBJECT, VAL_RETAINED, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// The global constants the reference installs from its post-registration
/// script (`StoragesFstatPreScript`).
const FILE_ATTRIBUTE_PRE_SCRIPT: &str = "\
global.FILE_ATTRIBUTE_READONLY = 0x00000001;\n\
global.FILE_ATTRIBUTE_HIDDEN = 0x00000002;\n\
global.FILE_ATTRIBUTE_SYSTEM = 0x00000004;\n\
global.FILE_ATTRIBUTE_DIRECTORY = 0x00000010;\n\
global.FILE_ATTRIBUTE_ARCHIVE = 0x00000020;\n\
global.FILE_ATTRIBUTE_NORMAL = 0x00000080;\n\
global.FILE_ATTRIBUTE_TEMPORARY = 0x00000100;";

/// Per-engine `fstat` dispatch state (kept out of process globals).
#[derive(Default)]
struct FstatState {
    /// Counter mixed into [`get_temporary_name`] so repeated calls differ.
    temp_counter: u64,
}

// ---------------------------------------------------------------------------
// Resolution / marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the installed engine → context binding, or report and return.
macro_rules! resolve_or_return {
    ($engine:expr, $out_error:expr, $method:expr) => {
        match resolve($engine) {
            Some(resolved) => resolved,
            None => {
                return report_error(
                    $out_error,
                    &format!("Storages.{}: compat context is not bound", $method),
                );
            }
        }
    };
}

/// Resolve the attached L2 storage, or report and return.
macro_rules! require_storage {
    ($services:expr, $out_error:expr, $method:expr) => {
        match $services.storage() {
            Some(storage) => storage,
            None => {
                return report_error(
                    $out_error,
                    &format!("Storages.{}: no storage attached", $method),
                );
            }
        }
    };
}

/// `TVPNormalizeStorageName` for the common case: ASCII-lowercase and `\` → `/`.
fn normalize_name(name: &str) -> String {
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

/// The reference `TVPGetLocalName`: the OS path for a storage name, whether
/// or not it exists. A name with no locally accessible form (in-archive, or
/// an absolute POSIX path outside the game directory) raises the reference's
/// `TVPCannotGetLocalName` (`"Cannot get local name from %1"`,
/// `reference/cpp/core/base/MsgIntfInc.h:35`,
/// `reference/cpp/core/base/StorageIntf.cpp:546-550`).
fn local_path(storage: &Storage, name: &str) -> Result<PathBuf, String> {
    match storage.find(name) {
        Ok(Some(Location::Disk(path))) => Ok(path),
        Ok(Some(Location::Archive(_, _))) => Err(format!("Cannot get local name from {name}")),
        Ok(None) => {
            let relative = storage
                .relative_disk_name(name)
                .ok_or_else(|| format!("Cannot get local name from {name}"))?;
            Ok(storage.game_dir().join(relative))
        }
        // `Location` is `#[non_exhaustive]`; future variants have no local path.
        Ok(Some(_)) => Err(format!("Cannot get local name from {name}")),
        Err(error) => Err(error.to_string()),
    }
}

/// The reference `TVPGetPlacedPath`: a disk path only when the name resolves
/// to an existing disk file (archive and placer-only names yield `None`).
fn placed_local(storage: &Storage, name: &str) -> Option<PathBuf> {
    match storage.find(name) {
        Ok(Some(Location::Disk(path))) => Some(path),
        _ => None,
    }
}

/// Resolve a name against the game directory after normalization (`to` /
/// `copyFile` semantics).
fn normalized_local(storage: &Storage, name: &str) -> Option<PathBuf> {
    let normalized = normalize_name(name);
    storage
        .relative_disk_name(&normalized)
        .map(|relative| storage.game_dir().join(relative))
}

/// Resolve a name against the game directory without normalization
/// (`copyFileNoNormalize` semantics).
fn raw_local(storage: &Storage, name: &str) -> Option<PathBuf> {
    storage
        .relative_disk_name(name)
        .map(|relative| storage.game_dir().join(relative))
}

/// Evaluate a small object expression (`%[]`, `[]`, `new Date()`) and retain
/// the resulting object.
fn retained_object(engine: &Tjs2Engine, expression: &str) -> Result<DetachedValue, String> {
    match engine
        .eval_retained(expression, "fstat")
        .map_err(|error| error.to_string())?
    {
        RetainedValue::Object(object) => Ok(object),
        RetainedValue::Value(value) => Err(format!(
            "fstat: expression {expression:?} did not yield an object ({value:?})"
        )),
    }
}

/// Write a retained object into `out` as `VAL_RETAINED`; the C++ trampoline
/// consumes the retention when it copies the result.
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id() as usize;
        }
    }
    // The C++ conversion consumes the id; forgetting the wrapper avoids a
    // redundant (but safe) release.
    std::mem::forget(object);
}

/// Seconds since the Unix epoch, negative for pre-epoch times.
fn system_time_seconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs())
            .ok()
            .map(|seconds| -seconds),
    }
}

/// Disk metadata used by `fstat` / `getTime`.
struct DiskInfo {
    size: u64,
    is_dir: bool,
    mtime: Option<i64>,
    ctime: Option<i64>,
    atime: Option<i64>,
}

/// `TVP_stat` plus the reference's `getFileTime` fields. A failed stat is a
/// zero-valued "file" (the reference ignores `TVP_stat`'s return value), so
/// [`fstat`] reports `size: 0` and `void` timestamps for a missing name.
fn disk_info(path: &Path) -> DiskInfo {
    match std::fs::metadata(path) {
        Ok(meta) => DiskInfo {
            size: meta.len(),
            is_dir: meta.is_dir(),
            mtime: meta.modified().ok().and_then(system_time_seconds),
            ctime: meta.created().ok().and_then(system_time_seconds),
            atime: meta.accessed().ok().and_then(system_time_seconds),
        },
        Err(_) => DiskInfo {
            size: 0,
            is_dir: false,
            mtime: None,
            ctime: None,
            atime: None,
        },
    }
}

/// The reference `storeDate`: write a `Date` for `filetime` into `key`.
/// Epoch (`0`) and absent times are omitted, matching `if(filetime > 0)`.
/// The value is passed to `Date.setTime` **verbatim** — no second/millisecond
/// conversion — so `Date.getTime()` reads the file time back exactly
/// (`reference/cpp/plugins/fstat/main.cpp:112-133`,
/// `reference/cpp/core/tjs2/tjsDate.cpp:209-219`).
fn set_date_member(
    engine: &Tjs2Engine,
    dict: Tjs2ValueId,
    key: &str,
    seconds: Option<i64>,
) -> Result<(), String> {
    let Some(seconds) = seconds.filter(|seconds| *seconds > 0) else {
        return Ok(());
    };
    let date = retained_object(engine, "new Date()")?;
    engine.call_member(date.raw_id(), "setTime", &[TjsValue::Integer(seconds)])?;
    engine.set_member(
        dict,
        key,
        &TjsValue::Retained(date.raw_id() as usize as u64),
    )?;
    // The write consumed the retained id.
    std::mem::forget(date);
    Ok(())
}

/// Build the `size`/`mtime`/`ctime`/`atime` dictionary the reference's
/// `_getTime` returns. `size` is omitted for directories and by `getTime`.
fn build_time_dict(
    engine: &Tjs2Engine,
    size: Option<u64>,
    times: &[(&str, Option<i64>)],
) -> Result<DetachedValue, String> {
    let dict = retained_object(engine, "%[]")?;
    if let Some(size) = size {
        let size = i64::try_from(size).unwrap_or(i64::MAX);
        engine.set_member(dict.raw_id(), "size", &TjsValue::Integer(size))?;
    }
    for (key, seconds) in times {
        set_date_member(engine, dict.raw_id(), key, *seconds)?;
    }
    Ok(dict)
}

/// The reference `restoreDate`: read the `Date`-valued member through
/// `Date.getTime()` and return the value **verbatim** — the reference
/// passes it straight to `TVP_utime` (`hasC ? m : 0`), i.e. neither side
/// converts between seconds and milliseconds
/// (`reference/cpp/plugins/fstat/main.cpp:135-148,215-218`).
fn restore_date_filetime(engine: &Tjs2Engine, dict: Tjs2ValueId, member: &str) -> Option<i64> {
    let value = engine.get_member(dict, member).ok()?;
    if !matches!(value, TjsValue::Object) {
        return None;
    }
    let date = engine.retain_value_detached(&TjsValue::Object).ok()?;
    let filetime = match engine.call_member(date.raw_id(), "getTime", &[]).ok()? {
        TjsValue::Integer(value) => value,
        TjsValue::Real(value) => value as i64,
        _ => return None,
    };
    Some(filetime)
}

/// `TVP_utime`: set both the access and modification times, in seconds.
fn set_file_times(path: &Path, seconds: i64) -> std::io::Result<()> {
    let when = if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    };
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_times(
        std::fs::FileTimes::new()
            .set_accessed(when)
            .set_modified(when),
    )
}

/// POSIX permission bits (the reference `static_cast<int32_t>(perms)`).
#[cfg(unix)]
fn permissions_mode(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::PermissionsExt;
    i64::from(meta.permissions().mode() & 0o7777)
}

/// Non-Unix fallback: the read-only bit is all the portable layer exposes.
#[cfg(not(unix))]
fn permissions_mode(meta: &std::fs::Metadata) -> i64 {
    i64::from(meta.permissions().readonly())
}

// ---------------------------------------------------------------------------
// `Storages` members
// ---------------------------------------------------------------------------

/// `Storages.clearStorageCaches()` — the reference `TVPClearStorageCaches`.
/// The L2 mounts parse their indexes eagerly and hold no lookup cache, so the
/// only L3 cache (the sibling port's placed-path cache) is not reachable from
/// here; the call is a documented no-op.
extern "C" fn storages_clear_storage_caches(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "clearStorageCaches");
    set_void(out);
    0
}

/// `Storages.fstat(filename)` — size and `Date` timestamps. Archive entries
/// return only the uncompressed `size`.
extern "C" fn storages_fstat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "fstat");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "fstat");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(out_error, "Storages.fstat requires 1 argument");
    };
    let name = value_as_string(name);

    match storage.find(&name) {
        Ok(Some(Location::Archive(..))) => {
            let size = match storage.stat(&name) {
                Ok(Some(meta)) => meta.size,
                Ok(None) => {
                    return report_error(
                        out_error,
                        &format!("Storages.fstat: storage not found: {name}"),
                    );
                }
                Err(error) => {
                    return report_error(out_error, &format!("Storages.fstat: {error}"));
                }
            };
            match build_time_dict(host, Some(size), &[]) {
                Ok(dict) => {
                    set_retained_out(out, dict);
                    0
                }
                Err(error) => report_error(out_error, &format!("Storages.fstat: {error}")),
            }
        }
        Ok(_) => {
            let path = match local_path(storage, &name) {
                Ok(path) => path,
                Err(error) => {
                    return report_error(out_error, &format!("Storages.fstat: {error}"));
                }
            };
            let info = disk_info(&path);
            let size = (!info.is_dir).then_some(info.size);
            let times = [
                ("mtime", info.mtime),
                ("ctime", info.ctime),
                ("atime", info.atime),
            ];
            match build_time_dict(host, size, &times) {
                Ok(dict) => {
                    set_retained_out(out, dict);
                    0
                }
                Err(error) => report_error(out_error, &format!("Storages.fstat: {error}")),
            }
        }
        Err(error) => report_error(out_error, &format!("Storages.fstat: {error}")),
    }
}

/// `Storages.getTime(target)` — the reference `_getTime` with no size.
extern "C" fn storages_get_time(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getTime");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "getTime");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(out_error, "Storages.getTime requires 1 argument");
    };
    let name = value_as_string(name);
    let path = match local_path(storage, &name) {
        Ok(path) => path,
        Err(error) => return report_error(out_error, &format!("Storages.getTime: {error}")),
    };
    let info = disk_info(&path);
    let times = [
        ("mtime", info.mtime),
        ("ctime", info.ctime),
        ("atime", info.atime),
    ];
    match build_time_dict(host, None, &times) {
        Ok(dict) => {
            set_retained_out(out, dict);
            0
        }
        Err(error) => report_error(out_error, &format!("Storages.getTime: {error}")),
    }
}

/// `Storages.setTime(target, times)` — apply the dictionary's `mtime` Date
/// (the reference only restores `mtime`). Always reports success, as the
/// reference does.
///
/// When the dictionary holds no usable `Date` the reference still calls
/// `TVP_utime(..., 0)` (`hasC ? m : 0`), clobbering the file's times to the
/// epoch; reproduced here
/// (`reference/cpp/plugins/fstat/main.cpp:215-218`).
extern "C" fn storages_set_time(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "setTime");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "setTime");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 2 {
        return report_error(out_error, "Storages.setTime requires 2 arguments");
    }
    let name = value_as_string(&argv[0]);
    let path = match local_path(storage, &name) {
        Ok(path) => path,
        Err(error) => return report_error(out_error, &format!("Storages.setTime: {error}")),
    };
    if argv[1].ty == VAL_OBJECT {
        match host.retain_object_arg(&argv[1]) {
            Ok(dict) => {
                // No usable `Date` → the reference passes `0` to `TVP_utime`.
                let filetime = restore_date_filetime(host, dict.raw_id(), "mtime").unwrap_or(0);
                if let Err(error) = set_file_times(&path, filetime) {
                    log::warn!("Storages.setTime: {} failed: {error}", path.display());
                }
            }
            Err(error) => {
                // `AsObjectNoAddRef()` yields null → `restoreDate` fails → epoch.
                log::warn!("Storages.setTime: invalid time dictionary: {error}");
                if let Err(error) = set_file_times(&path, 0) {
                    log::warn!("Storages.setTime: {} failed: {error}", path.display());
                }
            }
        }
    } else {
        // Not an object at all → `restoreDate` fails → epoch.
        if let Err(error) = set_file_times(&path, 0) {
            log::warn!("Storages.setTime: {} failed: {error}", path.display());
        }
    }
    set_bool(out, true);
    0
}

/// `Storages.getLastModifiedFileTime(target)` — always `0`.
///
/// The reference inverts its own stat check: `TVP_stat` returns **true on
/// success** (`reference/cpp/core/environ/linux/Platform.cpp:252-264`,
/// `Platform.h:69`), so `if(TVP_stat(...)) return 0;` returns `0` for every
/// existing file — and a failed stat falls through to the zero-initialised
/// `tTVP_stat stat{}`, which is `0` as well
/// (`reference/cpp/plugins/fstat/main.cpp:331-338`). The manual promises a
/// timestamp here (`fstat/manaul.tjs:176-184`), but the shipped
/// implementation never returns one; this port pins the shipped behaviour.
extern "C" fn storages_get_last_modified_file_time(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getLastModifiedFileTime");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "getLastModifiedFileTime");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.is_empty() {
        return report_error(
            out_error,
            "Storages.getLastModifiedFileTime requires 1 argument",
        );
    }
    // The argument is validated (normalize + local name) exactly like the
    // reference before its inverted check discards the stat result.
    let name = value_as_string(&argv[0]);
    if let Err(error) = local_path(storage, &name) {
        return report_error(
            out_error,
            &format!("Storages.getLastModifiedFileTime: {error}"),
        );
    }
    set_int(out, 0);
    0
}

/// `Storages.setLastModifiedFileTime(target, time)` — the reference
/// `TVP_utime` on the target with `time` in seconds.
extern "C" fn storages_set_last_modified_file_time(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "setLastModifiedFileTime");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "setLastModifiedFileTime");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 2 {
        return report_error(
            out_error,
            "Storages.setLastModifiedFileTime requires 2 arguments",
        );
    }
    let name = value_as_string(&argv[0]);
    let path = match local_path(storage, &name) {
        Ok(path) => path,
        Err(error) => {
            return report_error(
                out_error,
                &format!("Storages.setLastModifiedFileTime: {error}"),
            );
        }
    };
    let seconds = value_as_i64(&argv[1]);
    let ok = set_file_times(&path, seconds).is_ok();
    if !ok {
        log::warn!(
            "Storages.setLastModifiedFileTime: {} failed",
            path.display()
        );
    }
    set_bool(out, ok);
    0
}

/// `Storages.exportFile(filename, storename)` — copy a storage entry to a
/// local file, raising the reference's `cannot open readfile` /
/// `cannot open storefile` errors.
extern "C" fn storages_export_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "exportFile");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "exportFile");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 2 {
        return report_error(out_error, "Storages.exportFile requires 2 arguments");
    }
    let source = value_as_string(&argv[0]);
    let dest = value_as_string(&argv[1]);
    let bytes = match storage.read(&source) {
        Ok(bytes) => bytes,
        Err(error) => {
            return report_error(
                out_error,
                &format!("Storages.exportFile: cannot open readfile: {source} ({error})"),
            );
        }
    };
    let path = match local_path(storage, &dest) {
        Ok(path) => path,
        Err(error) => return report_error(out_error, &format!("Storages.exportFile: {error}")),
    };
    if let Err(error) = std::fs::write(&path, &bytes) {
        return report_error(
            out_error,
            &format!("Storages.exportFile: cannot open storefile: {dest} ({error})"),
        );
    }
    set_void(out);
    0
}

/// `Storages.deleteFile(file)` — delete an existing disk file. Archive
/// entries cannot be deleted (the reference only touches local files).
extern "C" fn storages_delete_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "deleteFile");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "deleteFile");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(out_error, "Storages.deleteFile requires 1 argument");
    };
    let name = value_as_string(name);
    let removed = match placed_local(storage, &name) {
        Some(path) => match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("Storages.deleteFile: {} failed: {error}", path.display());
                false
            }
        },
        None => false,
    };
    set_bool(out, removed);
    0
}

/// `Storages.truncateFile(file, size)` — a reference no-op that warns and
/// reports success on non-Windows hosts.
extern "C" fn storages_truncate_file(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "truncateFile");
    if argc < 2 {
        return report_error(out_error, "Storages.truncateFile requires 2 arguments");
    }
    log::warn!("Storages.truncateFile: not implemented by the reference; returning true");
    set_bool(out, true);
    0
}

/// `Storages.moveFile(from, to)` — copy then delete, as the reference does.
extern "C" fn storages_move_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "moveFile");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "moveFile");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 2 {
        return report_error(out_error, "Storages.moveFile requires 2 arguments");
    }
    let from = value_as_string(&argv[0]);
    let to = value_as_string(&argv[1]);
    let moved = match (local_path(storage, &from), local_path(storage, &to)) {
        (Ok(source), Ok(dest)) if source.is_file() => {
            match std::fs::copy(&source, &dest).and_then(|_| std::fs::remove_file(&source)) {
                Ok(()) => true,
                Err(error) => {
                    log::warn!(
                        "Storages.moveFile: {} -> {} failed: {error}",
                        source.display(),
                        dest.display()
                    );
                    false
                }
            }
        }
        _ => false,
    };
    set_bool(out, moved);
    0
}

/// `Storages.dirlist(dir)` — the immediate child names (no trailing `/`).
extern "C" fn storages_dirlist(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "dirlist");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "dirlist");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(out_error, "Storages.dirlist requires 1 argument");
    };
    let dir = value_as_string(dir);
    let path = match directory_path(storage, &dir, out_error, "dirlist") {
        Ok(path) => path,
        Err(code) => return code,
    };
    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(_) => return report_error(out_error, "Storages.dirlist: Directory not found."),
    };
    let array = match retained_object(host, "[]") {
        Ok(array) => array,
        Err(error) => return report_error(out_error, &format!("Storages.dirlist: {error}")),
    };
    for (index, entry) in entries.flatten().enumerate() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Err(error) =
            host.set_member(array.raw_id(), &index.to_string(), &TjsValue::String(name))
        {
            return report_error(out_error, &format!("Storages.dirlist: {error}"));
        }
    }
    set_retained_out(out, array);
    0
}

/// `Storages.dirlistEx(dir)` — immediate children as
/// `%[name, size, attrib, mtime, atime, ctime]` dictionaries.
extern "C" fn storages_dirlist_ex(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "dirlistEx");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "dirlistEx");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(out_error, "Storages.dirlistEx requires 1 argument");
    };
    let dir = value_as_string(dir);
    let path = match directory_path(storage, &dir, out_error, "dirlistEx") {
        Ok(path) => path,
        Err(code) => return code,
    };
    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(_) => return report_error(out_error, "Storages.dirlistEx: Directory not found."),
    };
    let array = match retained_object(host, "[]") {
        Ok(array) => array,
        Err(error) => return report_error(out_error, &format!("Storages.dirlistEx: {error}")),
    };
    for (index, entry) in entries.flatten().enumerate() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry.metadata().ok();
        let size = meta
            .as_ref()
            .filter(|meta| meta.is_file())
            .map_or(0, |meta| i64::try_from(meta.len()).unwrap_or(i64::MAX));
        let attrib = meta.as_ref().map_or(0, permissions_mode);
        // The reference writes raw epoch **seconds** (not `Date` objects,
        // unlike `_getTime`): `tTJSVariant vmtime = mtime;` with
        // `mtime = to_time_t(...)` and `ctime`/`atime` mirroring it
        // (`reference/cpp/plugins/fstat/main.cpp:584-597`).
        let mtime = meta
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .and_then(system_time_seconds)
            .unwrap_or(0);
        let dict = match retained_object(host, "%[]") {
            Ok(dict) => dict,
            Err(error) => return report_error(out_error, &format!("Storages.dirlistEx: {error}")),
        };
        for (key, value) in [
            ("name", TjsValue::String(name)),
            ("size", TjsValue::Integer(size)),
            ("attrib", TjsValue::Integer(attrib)),
            ("mtime", TjsValue::Integer(mtime)),
            ("ctime", TjsValue::Integer(mtime)),
            ("atime", TjsValue::Integer(mtime)),
        ] {
            if let Err(error) = host.set_member(dict.raw_id(), key, &value) {
                return report_error(out_error, &format!("Storages.dirlistEx: {error}"));
            }
        }
        if let Err(error) = host.set_member(
            array.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(dict.raw_id() as usize as u64),
        ) {
            return report_error(out_error, &format!("Storages.dirlistEx: {error}"));
        }
        // The array write consumed the dictionary's retained id.
        std::mem::forget(dict);
    }
    set_retained_out(out, array);
    0
}

/// `Storages.dirtree(dir, dironly=false)` — every descendant below `dir`,
/// directories suffixed `/`, recursively enumerated.
extern "C" fn storages_dirtree(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "dirtree");
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "dirtree");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(out_error, "Storages.dirtree requires 1 argument");
    };
    let dironly = argv.get(1).is_some_and(value_as_bool);
    let dir = format!("{}/", value_as_string(dir).trim_end_matches('/'));
    let path = match local_path(storage, &normalize_name(&dir)) {
        Ok(path) => path,
        Err(error) => return report_error(out_error, &format!("Storages.dirtree: {error}")),
    };
    let array = match retained_object(host, "[]") {
        Ok(array) => array,
        Err(error) => return report_error(out_error, &format!("Storages.dirtree: {error}")),
    };
    let mut count = 0i64;
    if let Err(error) = walk_tree(host, array.raw_id(), &path, "", &mut count, dironly) {
        return report_error(out_error, &format!("Storages.dirtree: {error}"));
    }
    set_retained_out(out, array);
    0
}

/// Reference `_dirtree`: recursion over `std::filesystem::directory_iterator`.
fn walk_tree(
    engine: &Tjs2Engine,
    array: Tjs2ValueId,
    dir: &Path,
    subdir: &str,
    count: &mut i64,
    dironly: bool,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let full = format!("{subdir}{name}/");
            engine.set_member(array, &count.to_string(), &TjsValue::String(full.clone()))?;
            *count += 1;
            walk_tree(engine, array, &entry.path(), &full, count, dironly)?;
        } else if !dironly {
            engine.set_member(
                array,
                &count.to_string(),
                &TjsValue::String(format!("{subdir}{name}")),
            )?;
            *count += 1;
        }
    }
    Ok(())
}

/// `Storages.removeDirectory(dir)` — remove an empty directory.
extern "C" fn storages_remove_directory(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "removeDirectory");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "removeDirectory");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(out_error, "Storages.removeDirectory requires 1 argument");
    };
    let dir = value_as_string(dir);
    let path = match directory_path(storage, &dir, out_error, "removeDirectory") {
        Ok(path) => path,
        Err(code) => return code,
    };
    let removed = match std::fs::remove_dir(&path) {
        Ok(()) => true,
        Err(error) => {
            log::warn!(
                "Storages.removeDirectory: {} failed: {error}",
                path.display()
            );
            false
        }
    };
    set_bool(out, removed);
    0
}

/// `Storages.createDirectory(dir)` — create a directory (normalized name).
extern "C" fn storages_create_directory(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    storages_create_directory_impl(engine, argc, argv, out, out_error, true)
}

/// `Storages.createDirectoryNoNormalize(dir)` — create a directory without
/// normalizing the name.
extern "C" fn storages_create_directory_no_normalize(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    storages_create_directory_impl(engine, argc, argv, out, out_error, false)
}

/// Shared body of the two `createDirectory` members.
fn storages_create_directory_impl(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    normalize: bool,
) -> c_int {
    let method = if normalize {
        "createDirectory"
    } else {
        "createDirectoryNoNormalize"
    };
    let resolved = resolve_or_return!(engine, out_error, method);
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, method);
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(out_error, &format!("Storages.{method} requires 1 argument"));
    };
    let dir = value_as_string(dir);
    let path = if normalize {
        match directory_path(storage, &dir, out_error, method) {
            Ok(path) => path,
            Err(code) => return code,
        }
    } else {
        if !dir.ends_with('/') {
            return report_error(
                out_error,
                &format!(
                    "Storages.{method}: '/' must be specified at the end of given directory name."
                ),
            );
        }
        match raw_local(storage, &dir) {
            Some(path) => path,
            None => {
                return report_error(
                    out_error,
                    &format!("Storages.{method}: cannot resolve {dir}"),
                );
            }
        }
    };
    let existed = path.exists();
    let created = match std::fs::create_dir_all(&path) {
        Ok(()) => !existed,
        Err(error) => {
            log::warn!("Storages.{method}: {} failed: {error}", path.display());
            false
        }
    };
    set_bool(out, created);
    0
}

/// `Storages.changeDirectory(dir)` — the reference logs and reports success.
extern "C" fn storages_change_directory(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "changeDirectory");
    if argc < 1 {
        return report_error(out_error, "Storages.changeDirectory requires 1 argument");
    }
    log::warn!("Storages.changeDirectory: not implemented by the reference; returning true");
    set_bool(out, true);
    0
}

/// `Storages.setFileAttributes(filename, attr)` — reference no-op.
///
/// Both parameters are mandatory (`ncbind.hpp:1178` arity check against
/// `setFileAttributes(ttstr, DWORD)`, `reference/cpp/plugins/fstat/main.cpp:700`).
extern "C" fn storages_set_file_attributes(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "setFileAttributes");
    if argc < 2 {
        return report_error(out_error, "Storages.setFileAttributes requires 2 arguments");
    }
    log::warn!("Storages.setFileAttributes: not implemented by the reference; returning true");
    set_bool(out, true);
    0
}

/// `Storages.resetFileAttributes(filename, attr)` — reference no-op.
///
/// Both parameters are mandatory (`ncbind.hpp:1178`,
/// `reference/cpp/plugins/fstat/main.cpp:713`).
extern "C" fn storages_reset_file_attributes(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "resetFileAttributes");
    if argc < 2 {
        return report_error(
            out_error,
            "Storages.resetFileAttributes requires 2 arguments",
        );
    }
    log::warn!("Storages.resetFileAttributes: not implemented by the reference; returning true");
    set_bool(out, true);
    0
}

/// `Storages.getFileAttributes(filename)` — reference no-op returning `0`.
extern "C" fn storages_get_file_attributes(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "getFileAttributes");
    if argc < 1 {
        return report_error(out_error, "Storages.getFileAttributes requires 1 argument");
    }
    log::warn!("Storages.getFileAttributes: not implemented by the reference; returning 0");
    set_int(out, 0);
    0
}

/// `Storages.selectDirectory(params)` — the reference validates one argument,
/// warns and returns `void` (the folder chooser is not implemented).
extern "C" fn storages_select_directory(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _resolved = resolve_or_return!(engine, out_error, "selectDirectory");
    if argc < 1 {
        return report_error(out_error, "Storages.selectDirectory requires 1 argument");
    }
    log::warn!("Storages.selectDirectory: not implemented by the reference");
    set_void(out);
    0
}

/// `Storages.isExistentDirectory(dir)` — whether `dir` is an existing
/// directory.
extern "C" fn storages_is_existent_directory(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "isExistentDirectory");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "isExistentDirectory");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(dir) = argv.first() else {
        return report_error(
            out_error,
            "Storages.isExistentDirectory requires 1 argument",
        );
    };
    let dir = value_as_string(dir);
    let path = match directory_path(storage, &dir, out_error, "isExistentDirectory") {
        Ok(path) => path,
        Err(code) => return code,
    };
    set_bool(out, path.is_dir());
    0
}

/// `Storages.copyFile(from, to)` — copy through the placed/resolved paths.
extern "C" fn storages_copy_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    storages_copy_file_impl(engine, argc, argv, out, out_error, true)
}

/// `Storages.copyFileNoNormalize(from, to, failIfExist)` — copy without
/// normalizing `to`. The reference accepts `failIfExist` but ignores it.
extern "C" fn storages_copy_file_no_normalize(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    storages_copy_file_impl(engine, argc, argv, out, out_error, false)
}

/// Shared body of the two `copyFile` members.
///
/// `copyFile(from, to)` takes two parameters, `copyFileNoNormalize(from, to,
/// failIfExist)` three (`reference/cpp/plugins/fstat/main.cpp:774,800`) —
/// ncbind rejects shorter calls (`ncbind.hpp:1178`).
fn storages_copy_file_impl(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    normalize: bool,
) -> c_int {
    let method = if normalize {
        "copyFile"
    } else {
        "copyFileNoNormalize"
    };
    let required = if normalize { 2 } else { 3 };
    let resolved = resolve_or_return!(engine, out_error, method);
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, method);
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if argv.len() < required {
        return report_error(
            out_error,
            &format!("Storages.{method} requires {required} arguments"),
        );
    }
    let from = value_as_string(&argv[0]);
    let to = value_as_string(&argv[1]);
    let source = placed_local(storage, &from);
    let dest = if normalize {
        normalized_local(storage, &to)
    } else {
        raw_local(storage, &to)
    };
    let copied = match (source, dest) {
        (Some(source), Some(dest)) => match std::fs::copy(&source, &dest) {
            Ok(_) => true,
            Err(error) => {
                log::warn!(
                    "Storages.{method}: {} -> {} failed: {error}",
                    source.display(),
                    dest.display()
                );
                false
            }
        },
        _ => false,
    };
    set_bool(out, copied);
    0
}

/// `Storages.isExistentStorageNoSearchNoNormalize(filename)` — existence
/// without auto-path search and without normalizing the outer name.
extern "C" fn storages_is_existent_storage_no_search_no_normalize(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "isExistentStorageNoSearchNoNormalize");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "isExistentStorageNoSearchNoNormalize");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(
            out_error,
            "Storages.isExistentStorageNoSearchNoNormalize requires 1 argument",
        );
    };
    let name = value_as_string(name);
    let exists = if name.contains('>') {
        matches!(storage.find(&name), Ok(Some(_)))
    } else {
        let candidate = if Path::new(&name).is_absolute() {
            PathBuf::from(&name)
        } else {
            storage.game_dir().join(&name)
        };
        candidate.exists()
    };
    set_bool(out, exists);
    0
}

/// `Storages.getDisplayName(filename)` — the base file name of an existing
/// local file, or `""`.
extern "C" fn storages_get_display_name(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getDisplayName");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "getDisplayName");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(out_error, "Storages.getDisplayName requires 1 argument");
    };
    let name = value_as_string(name);
    let display = local_path(storage, &name)
        .ok()
        .filter(|path| path.exists())
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    set_string_out(&ctx, out, &display);
    0
}

/// `Storages.getMD5HashString(filename)` — lowercase hex MD5 of the storage
/// entry's bytes.
extern "C" fn storages_get_md5_hash_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getMD5HashString");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "getMD5HashString");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return report_error(out_error, "Storages.getMD5HashString requires 1 argument");
    };
    let name = value_as_string(name);
    let bytes = match storage.read(&name) {
        Ok(bytes) => bytes,
        Err(error) => {
            return report_error(
                out_error,
                &format!("Storages.getMD5HashString: cannot open : {name} ({error})"),
            );
        }
    };
    set_string_out(&ctx, out, &md5_hex(&bytes));
    0
}

/// `Storages.searchPath(filename, searchpath)` — search a `:`/`;`-separated
/// path list (the system `PATH` when omitted) for a regular file, returning
/// the normalized absolute path or `void`.
extern "C" fn storages_search_path(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "searchPath");
    let ctx = resolved.ctx();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "Storages.searchPath requires 1 argument");
    };
    let filename = value_as_string(filename);
    let searchpath = argv.get(1).map(value_as_string).unwrap_or_default();
    let list = if searchpath.is_empty() {
        std::env::var("PATH").unwrap_or_default()
    } else {
        searchpath
    };
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    for entry in list.split(delimiter).filter(|entry| !entry.is_empty()) {
        let candidate = Path::new(entry).join(&filename);
        if !candidate.is_file() {
            continue;
        }
        if let Ok(absolute) = std::path::absolute(&candidate) {
            set_string_out(&ctx, out, &normalize_name(&absolute.to_string_lossy()));
            return 0;
        }
    }
    set_void(out);
    0
}

/// Build the local path for a `dirlist`-style member, enforcing the
/// reference's trailing `'/'` rule.
fn directory_path(
    storage: &Storage,
    dir: &str,
    out_error: *mut *mut c_char,
    method: &str,
) -> Result<PathBuf, c_int> {
    if !dir.ends_with('/') {
        return Err(report_error(
            out_error,
            &format!(
                "Storages.{method}: '/' must be specified at the end of given directory name."
            ),
        ));
    }
    local_path(storage, &normalize_name(dir))
        .map_err(|error| report_error(out_error, &format!("Storages.{method}: {error}")))
}

/// `Storages.getTemporaryName()` — a unique name under the game directory.
extern "C" fn storages_get_temporary_name(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getTemporaryName");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "getTemporaryName");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    let app_path = storage.game_dir().to_string_lossy().into_owned();
    let separator = if app_path.ends_with('/') || app_path.ends_with('\\') {
        ""
    } else {
        "/"
    };
    let counter = {
        let mut state = services.state::<FstatState>();
        state.temp_counter = state.temp_counter.wrapping_add(1);
        state.temp_counter
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    let mut seed = nanos ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    seed ^= u64::from(std::process::id());
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    let pid = std::process::id();
    set_string_out(
        &ctx,
        out,
        &format!(
            "{app_path}{separator}krkr_{:012x}_{counter}_{pid}",
            seed & 0xFFFF_FFFF_FFFF
        ),
    );
    0
}

/// `Storages.currentPath` getter — the normalized game/storage path.
extern "C" fn storages_current_path_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "currentPath");
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let storage = require_storage!(services, out_error, "currentPath");
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    let mut path = normalize_name(&storage.game_dir().to_string_lossy());
    if !path.ends_with('/') {
        path.push('/');
    }
    set_string_out(&ctx, out, &path);
    0
}

/// `Storages.currentPath` setter — the reference forwards to
/// `changeDirectory`, which is a no-op on non-Windows hosts.
extern "C" fn storages_current_path_set(
    engine: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, _out_error, "currentPath");
    let _ = resolved;
    log::warn!("Storages.currentPath: changeDirectory is not implemented by the reference");
    0
}

// ---------------------------------------------------------------------------
// `TemporaryFiles` instance class
// ---------------------------------------------------------------------------

/// Payload for [`TemporaryFiles`]. The reference class validates but records
/// no entries, so this is a unit value.
struct TemporaryFilesInst;

/// `new TemporaryFiles()` — reference `Constructor()`.
extern "C" fn temporary_files_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TemporaryFilesInst)) as *mut c_void
}

/// Reference `~TemporaryFiles`: nothing to release.
extern "C" fn temporary_files_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `temporary_files_create` and is freed once.
    drop(unsafe { Box::from_raw(instance as *mut TemporaryFilesInst) });
}

/// `TemporaryFiles.TemporaryFiles()` — the reference `Constructor()`.
///
/// Registering the class-name member is what lets a script subclass reach the
/// native instance through `super.TemporaryFiles()`; the ABI installs the
/// constructor dispatch only for a class-named method.
extern "C" fn temporary_files_ctor(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "TemporaryFiles: compat context is not bound");
    }
    set_void(out);
    0
}

/// `TemporaryFiles.entry(filename)` — true for an existing readable regular
/// file.
extern "C" fn temporary_files_entry(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let valid = temporary_files_check(engine, argc, argv, out_error, "entry", false);
    match valid {
        Ok(valid) => {
            set_bool(out, valid);
            0
        }
        Err(code) => code,
    }
}

/// `TemporaryFiles.entryFolder(foldername)` — true for an existing directory.
extern "C" fn temporary_files_entry_folder(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let valid = temporary_files_check(engine, argc, argv, out_error, "entryFolder", true);
    match valid {
        Ok(valid) => {
            set_bool(out, valid);
            0
        }
        Err(code) => code,
    }
}

/// Shared body of the two `TemporaryFiles` validators.
fn temporary_files_check(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out_error: *mut *mut c_char,
    method: &str,
    folder: bool,
) -> Result<bool, c_int> {
    let Some(resolved) = resolve(engine) else {
        return Err(report_error(
            out_error,
            &format!("TemporaryFiles.{method}: compat context is not bound"),
        ));
    };
    let ctx = resolved.ctx();
    // SAFETY: only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return Err(report_error(
            out_error,
            &format!("TemporaryFiles.{method}: no storage attached"),
        ));
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(name) = argv.first() else {
        return Err(report_error(
            out_error,
            &format!("TemporaryFiles.{method} requires 1 argument"),
        ));
    };
    let name = value_as_string(name);
    let Ok(path) = local_path(storage, &name) else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }
    if folder {
        return Ok(path.is_dir());
    }
    Ok(path.is_file() && std::fs::File::open(&path).is_ok())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: `ctx` is only created for a bound, live engine.
    unsafe { ctx.engine() }
        .exec_script(FILE_ATTRIBUTE_PRE_SCRIPT, "fstat")
        .map_err(|error| {
            CompatError::Register(format!(
                "fstat: could not install FILE_ATTRIBUTE_* globals: {error}"
            ))
        })?;

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Storages",
        methods: vec![
            NativeMethodDef {
                name: "clearStorageCaches",
                f: storages_clear_storage_caches,
            },
            NativeMethodDef {
                name: "fstat",
                f: storages_fstat,
            },
            NativeMethodDef {
                name: "getTime",
                f: storages_get_time,
            },
            NativeMethodDef {
                name: "setTime",
                f: storages_set_time,
            },
            NativeMethodDef {
                name: "getLastModifiedFileTime",
                f: storages_get_last_modified_file_time,
            },
            NativeMethodDef {
                name: "setLastModifiedFileTime",
                f: storages_set_last_modified_file_time,
            },
            NativeMethodDef {
                name: "exportFile",
                f: storages_export_file,
            },
            NativeMethodDef {
                name: "deleteFile",
                f: storages_delete_file,
            },
            NativeMethodDef {
                name: "truncateFile",
                f: storages_truncate_file,
            },
            NativeMethodDef {
                name: "moveFile",
                f: storages_move_file,
            },
            NativeMethodDef {
                name: "dirlist",
                f: storages_dirlist,
            },
            NativeMethodDef {
                name: "dirlistEx",
                f: storages_dirlist_ex,
            },
            NativeMethodDef {
                name: "dirtree",
                f: storages_dirtree,
            },
            NativeMethodDef {
                name: "removeDirectory",
                f: storages_remove_directory,
            },
            NativeMethodDef {
                name: "createDirectory",
                f: storages_create_directory,
            },
            NativeMethodDef {
                name: "createDirectoryNoNormalize",
                f: storages_create_directory_no_normalize,
            },
            NativeMethodDef {
                name: "changeDirectory",
                f: storages_change_directory,
            },
            NativeMethodDef {
                name: "setFileAttributes",
                f: storages_set_file_attributes,
            },
            NativeMethodDef {
                name: "resetFileAttributes",
                f: storages_reset_file_attributes,
            },
            NativeMethodDef {
                name: "getFileAttributes",
                f: storages_get_file_attributes,
            },
            NativeMethodDef {
                name: "selectDirectory",
                f: storages_select_directory,
            },
            NativeMethodDef {
                name: "isExistentDirectory",
                f: storages_is_existent_directory,
            },
            NativeMethodDef {
                name: "copyFile",
                f: storages_copy_file,
            },
            NativeMethodDef {
                name: "copyFileNoNormalize",
                f: storages_copy_file_no_normalize,
            },
            NativeMethodDef {
                name: "isExistentStorageNoSearchNoNormalize",
                f: storages_is_existent_storage_no_search_no_normalize,
            },
            NativeMethodDef {
                name: "getDisplayName",
                f: storages_get_display_name,
            },
            NativeMethodDef {
                name: "getMD5HashString",
                f: storages_get_md5_hash_string,
            },
            NativeMethodDef {
                name: "searchPath",
                f: storages_search_path,
            },
            NativeMethodDef {
                name: "getTemporaryName",
                f: storages_get_temporary_name,
            },
        ],
        properties: vec![NativePropertyDef {
            name: "currentPath",
            get: Some(storages_current_path_get),
            set: Some(storages_current_path_set),
        }],
    })?;

    ctx.register_instance(&NativeInstanceBuilder {
        name: "TemporaryFiles",
        create: temporary_files_create,
        destroy: temporary_files_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "TemporaryFiles",
                f: temporary_files_ctor,
            },
            NativeInstanceMethodDef {
                name: "entry",
                f: temporary_files_entry,
            },
            NativeInstanceMethodDef {
                name: "entryFolder",
                f: temporary_files_entry_folder,
            },
        ],
        properties: vec![],
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
//
// The integration test in `tests/fstat.rs` drives the full aggregated VM,
// where sibling plugins registered later can shadow a member. These unit tests
// register only the core `Storages` class and this plugin, so they verify the
// reference semantics of *this* implementation (Date-valued timestamps,
// `TemporaryFiles`, the directory helpers).

#[cfg(test)]
mod tests {
    use super::register;
    use crate::{CompatCtx, CompatHost, CompatServices};
    use engine::Storage;
    use fs::FileSystem;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tjs::{Tjs2Engine, TjsValue};

    /// A bound VM with a mounted temp directory and only `Storages` + `fstat`
    /// registered.
    struct Harness {
        engine: Box<Tjs2Engine>,
        _host: CompatHost,
        _storage: Box<Storage>,
        dir: PathBuf,
    }

    impl Harness {
        fn new() -> Harness {
            let dir = std::env::temp_dir().join(format!(
                "krkr-fstat-unit-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let mut storage = Box::new(
                Storage::mount_archives(
                    FileSystem::host(&dir),
                    dir.clone(),
                    std::iter::empty::<&str>(),
                )
                .expect("mount storage"),
            );
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let mut host = CompatHost::new();
            // SAFETY: `storage` is boxed and outlives the binding, which this
            // test removes before the harness fields drop.
            unsafe {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage)
            };
            let services = host.services_mut() as *mut CompatServices;
            crate::binding::install(&engine, services, host.registry()).expect("install binding");
            {
                // SAFETY: `engine` and `services` are live for the harness body.
                let mut ctx = unsafe {
                    CompatCtx::from_raw(&*engine as *const Tjs2Engine as usize, services as usize)
                };
                crate::natives::storages::register(&mut ctx).expect("register Storages");
                register(&mut ctx).expect("register fstat");
            }
            Harness {
                engine,
                _host: host,
                _storage: storage,
                dir,
            }
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture parent");
            }
            std::fs::write(path, bytes).expect("write fixture");
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn set_mtime(&self, name: &str, seconds: i64) {
            let when = UNIX_EPOCH + Duration::from_secs(seconds as u64);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(self.path(name))
                .expect("open fixture");
            file.set_times(std::fs::FileTimes::new().set_modified(when))
                .expect("set mtime");
        }

        fn eval(&self, expression: &str) -> TjsValue {
            self.engine
                .eval(expression, "fstat-unit")
                .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
        }

        fn int(&self, expression: &str) -> i64 {
            match self.eval(expression) {
                TjsValue::Integer(value) => value,
                TjsValue::Real(value) => value as i64,
                other => panic!("{expression:?} returned {other:?}, expected an integer"),
            }
        }

        fn run(&self, script: &str) {
            self.engine
                .exec_script(script, "fstat-unit")
                .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            crate::binding::uninstall();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn fstat_returns_date_timestamps() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Harness::new();
        env.write("a.txt", b"hello");
        env.set_mtime("a.txt", 1_600_000_000);

        assert_eq!(env.int("Storages.fstat('a.txt').size"), 5);
        assert_eq!(
            env.eval("typeof Storages.fstat('a.txt').mtime"),
            TjsValue::String("Object".into())
        );
        // `storeDate` hands `Date.setTime` the raw `st_mtime` epoch seconds
        // (`reference/cpp/plugins/fstat/main.cpp:112-133,198`), which
        // `setTime` divides by 1000 and `getTime` multiplies back
        // (`reference/cpp/core/tjs2/tjsDate.cpp:216,305`), so `getTime()`
        // equals the file's epoch seconds
        // (`reference/cpp/core/environ/linux/Platform.cpp:261`).
        assert_eq!(
            env.int("Storages.fstat('a.txt').mtime.getTime()"),
            1_600_000_000
        );

        // A missing disk name is still a zero-size entry with void timestamps.
        assert_eq!(env.int("Storages.fstat('missing.txt').size"), 0);
        assert_eq!(
            env.eval("typeof Storages.getTime('missing.txt').mtime"),
            TjsValue::String("undefined".into())
        );
    }

    #[test]
    fn get_time_and_set_time_round_trip() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Harness::new();
        env.write("a.txt", b"hello");
        env.set_mtime("a.txt", 1_700_000_000);

        // `getTime` reports the same raw `st_mtime` epoch seconds through
        // `Date` (`reference/cpp/plugins/fstat/main.cpp:112-133,197-198`;
        // `reference/cpp/core/tjs2/tjsDate.cpp:216,305`).
        assert_eq!(
            env.int("Storages.getTime('a.txt').mtime.getTime()"),
            1_700_000_000
        );
        assert_eq!(
            env.eval("typeof Storages.getTime('a.txt').size"),
            TjsValue::String("undefined".into())
        );

        env.run(
            "var times = %[]; \
             times.mtime = new Date(); times.mtime.setTime(1234567890000);",
        );
        assert_eq!(env.int("Storages.setTime('a.txt', times)"), 1);
        // `TVP_stat` returns true on success, so the reference's inverted
        // check returns `0` for every existing file
        // (`reference/cpp/plugins/fstat/main.cpp:331-338`).
        assert_eq!(env.int("Storages.getLastModifiedFileTime('a.txt')"), 0);
        assert_eq!(
            env.int("Storages.setLastModifiedFileTime('a.txt', 1500000000)"),
            1
        );
        assert_eq!(env.int("Storages.getLastModifiedFileTime('a.txt')"), 0);
    }

    #[test]
    fn temporary_files_validates_entries() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Harness::new();
        env.write("a.txt", b"a");
        std::fs::create_dir_all(env.path("folder")).expect("create folder");

        env.run("var tf = new TemporaryFiles();");
        assert_eq!(env.int("tf.entry('a.txt')"), 1);
        assert_eq!(env.int("tf.entry('folder')"), 0);
        assert_eq!(env.int("tf.entry('missing.txt')"), 0);
        assert_eq!(env.int("tf.entryFolder('folder')"), 1);
        assert_eq!(env.int("tf.entryFolder('a.txt')"), 0);
    }

    #[test]
    fn subclass_constructs_through_the_class_name_member() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Harness::new();
        env.write("a.txt", b"a");
        // `NCB_REGISTER_CLASS(TemporaryFiles)` declares `Constructor()`
        // (`reference/cpp/plugins/fstat/main.cpp:987`), so a script subclass
        // reaches the native instance through `super.TemporaryFiles()`; a
        // class registered without a class-named method gets no constructor
        // dispatch and the call raises.
        env.run(
            "class SubTemporaryFiles extends TemporaryFiles { \
                 function SubTemporaryFiles() { super.TemporaryFiles(); } \
             } \
             var tf = new SubTemporaryFiles();",
        );
        assert_eq!(env.int("tf.entry('a.txt')"), 1);
    }

    #[test]
    fn directory_helpers_walk_the_tree() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Harness::new();
        env.write("data/a.txt", b"a");
        env.write("data/sub/c.txt", b"ccc");
        std::fs::create_dir_all(env.path("data/empty")).expect("create empty dir");

        assert_eq!(env.int("Storages.dirlist('data/').length"), 3);
        assert_eq!(env.int("Storages.dirtree('data/').length"), 4);
        assert_eq!(env.int("Storages.isExistentDirectory('data/empty/')"), 1);
        assert_eq!(env.int("Storages.removeDirectory('data/empty/')"), 1);
        assert_eq!(env.int("Storages.isExistentDirectory('data/empty/')"), 0);
    }
}

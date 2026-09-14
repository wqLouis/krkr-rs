//! `Storages` native class (Rust port of the reference `tTJSNC_Storages`).
//!
//! This crate registers the TJS2 native class `Storages` — the static
//! storage-query API KiriKiri startup scripts use. It is backed by the
//! mounted game storage ([`engine::Storage`]) plus a process-global list of
//! auto search paths (maintained by `Storages.addAutoPath`).
//!
//! Ported from `reference/cpp/core/base/StorageIntf.cpp`
//! (`tTJSNC_Storages`, `TVPIsExistentStorage`, `TVPGetPlacedPath`,
//! `TVPAddAutoPath`/`TVPRemoveAutoPath`, the `TVPExtractStorage*` helpers)
//! and `reference/cpp/core/base/impl/StorageImpl.cpp` (`getLocalName`).
//!
//! # Implemented methods
//!
//! | method | behavior |
//! |---|---|
//! | `isExistentStorage(name)` | `true` when the name resolves in the
//!   mounted storage (disk file, mixed-case disk file, or `arc.xp3>path`
//!   archive entry) or is a file inside one of the auto search paths. |
//! | `getFileList(mask, attr)` | sorted, deduped storage names matching a
//!   wildcard mask — see [`get_file_list`] for the exact semantics. |
//! | `addAutoPath(path)` / `removeAutoPath(path)` | maintain the global
//!   auto-path list; like the reference, a path must end with `/`, `\` or
//!   `>` (`TVPMissingPathDelimiterAtLast`). |
//! | `getLocalName(name)` | absolute local disk path for a disk file; the
//!   name unchanged when no disk file exists; a TJS error for in-archive
//!   names (the reference throws `TVPCannotGetLocalName`). |
//! | `getFullPath(path)` | normalized storage name (lowercase, `/`
//!   separators). |
//! | `getPlacedPath(path)` | normalized name when found, `""` otherwise. |
//! | `extractStorageExt/Name/Path`, `chopStorageExt` | pure string helpers
//!   ported from the reference (they split on `/`, `\` and the `>` archive
//!   delimiter). |
//! | `clearArchiveCache()` | clears the positive placed-path cache and
//!   remounts the storage so every XP3 handle is released and lazily reopened
//!   (reference `TVPClearArchiveCache`). |
//! | `stat(name)` / `fstat(name)` | a TJS dictionary with disk size and Date
//!   timestamps, or archive-entry size. |
//! | `selectFile(param)` | no headless GUI dialog exists, so behaves like a
//!   user cancel: leaves `param` untouched and returns `false`. |
//! | `open(name[, flags])` | a real binary stream object
//!   (`read`/`write`/`seek`/`getSize`/`getPosition`/`close`); read mode must
//!   resolve in storage, write modes flush to the resolved disk path on
//!   `close`. |
//!
//! # Pending (registered, but raise a clear TJS error)
//!
//! - `searchCD(label)` — CD-volume search; disabled in the reference.
//!
//! # Return value of `getFileList`
//!
//! `getFileList` returns a real TJS `Array` of storage-name strings (the same
//! `VAL_ARRAY` marshaling `CSVParser.getNextLine` uses), sorted and deduped.
//!
//! # Process-global state
//!
//! [`STORAGE`] and [`AUTO_PATHS`] are process-wide; the engine calls
//! [`set_storage`] once before running `startup.tjs`. Tests share these
//! globals, so they must run single-threaded:
//! `cargo test -p tvp-storages -- --test-threads=1`.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Storage;
use engine::storage::StorageMetadata;
mod csv_parser;

use tjs2_sys::{
    Engine, NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMethodDef, NativeMethodDef,
    Tjs2Engine, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value, tjs2_free_string,
    tjs2_malloc,
};

// ---------------------------------------------------------------------------
// Process-global state
// ---------------------------------------------------------------------------

/// The mounted game storage. Set by the engine (via [`set_storage`]) before
/// running `startup.tjs`; all natives read it.
static STORAGE: Mutex<Option<Arc<Mutex<Storage>>>> = Mutex::new(None);

// The small part of the tjs2 ABI needed to construct a retained dictionary.
// These symbols are part of tjs2-sys' C ABI but are intentionally private in
// its safe Rust wrapper; declaring them here keeps this milestone scoped to
// the storage crates.
unsafe extern "C" {
    fn tjs2_eval(
        engine: *mut Engine,
        expression: *const c_char,
        name: *const c_char,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_retain_value(engine: *mut Engine, value: *const Value) -> *mut c_void;
}

/// Auto search paths registered via `Storages.addAutoPath` (normalized,
/// trailing `/` stripped).
/// Mount a game storage for the `Storages` class to query. Call once before
/// running `startup.tjs`; pass `None` to detach (used between tests).
pub fn set_storage(storage: Option<Arc<Mutex<Storage>>>) {
    log::debug!(
        "tvp-storages: storage {}",
        if storage.is_some() {
            "mounted"
        } else {
            "detached"
        }
    );
    *STORAGE.lock().unwrap() = storage;
}

fn storage_arc() -> Option<Arc<Mutex<Storage>>> {
    STORAGE.lock().unwrap().clone()
}

/// Positive-result cache for `TVPGetPlacedPath` (the reference's
/// `TVPAutoPathCache`): requested name -> placed normalized storage name.
/// Only hits are stored (the reference does not cache misses). It is
/// invalidated by `Storages.clearArchiveCache()` and whenever the auto-path
/// list changes, so it never serves a stale placement.
static PLACED_PATH_CACHE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn clear_placed_path_cache() {
    PLACED_PATH_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

// ---------------------------------------------------------------------------
// Name normalization and helpers
// ---------------------------------------------------------------------------

/// Normalize a storage name the way `TVPNormalizeStorageName` does for the
/// common case: ASCII-lowercase, `\` → `/`. (The reference also maps media
/// prefixes such as `file://./`; not needed for the mounted-game model.)
/// Resolve `base` (a storage name without directory) inside one auto path
/// entry. Entries may be disk dirs (`system/`) or archive prefixes
/// (`data.xp3>system/`); the latter are resolved through the mounted
/// storage, like the reference's auto-path table.
fn auto_path_resolves(entry: &str, base: &str) -> bool {
    storage_arc()
        .map(|st| {
            let joined = format!("{entry}{base}");
            st.lock().unwrap().find(&joined).is_some()
        })
        .unwrap_or(false)
}

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

/// Case-insensitive `*`/`?` wildcard match (ASCII case folding, like the
/// reference's normalized storage names).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut retry) = (usize::MAX, 0usize);
    while ti < text.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi].eq_ignore_ascii_case(&text[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = pi;
            retry = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            retry += 1;
            ti = retry;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Recursively list a directory as normalized storage names. Returns
/// `(name, is_dir, path)` where `name` is relative to `base`, lowercased and
/// with `/` separators (the reference lowercases every listed name).
fn disk_entries(base: &Path) -> Vec<(String, bool, PathBuf)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, bool, PathBuf)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if ft.is_dir() {
                out.push((rel.to_ascii_lowercase(), true, entry.path()));
                walk(&entry.path(), &rel, out);
            } else if ft.is_file() {
                out.push((rel.to_ascii_lowercase(), false, entry.path()));
            }
        }
    }
    let mut out = Vec::new();
    walk(base, "", &mut out);
    out
}

// ---------------------------------------------------------------------------
// Core storage queries
// ---------------------------------------------------------------------------

/// `TVPIsExistentStorageNoSearch`: does `name` resolve inside the mounted
/// storage (disk or archives), without consulting the auto paths?
fn exists_in_storage(name: &str) -> bool {
    let Some(storage) = storage_arc() else {
        return false;
    };
    // `name` may already be normalized (lowercased) by the caller; try the
    // raw name too so absolute disk paths with mixed case resolve on
    // case-sensitive filesystems.
    let found = {
        let st = storage.lock().unwrap();
        st.exists(name) || st.exists(&normalize_storage_name(name))
    };
    if found {
        return true;
    }
    // Fallback: `engine::Storage::find` is case-sensitive on disk, but
    // storage names are normalized to lowercase; scan for mixed-case files
    // (the reference lowercases listed names too).
    let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
    let normalized = normalize_storage_name(name);
    disk_entries(&game_dir)
        .iter()
        .any(|(n, is_dir, _)| !is_dir && n == &normalized)
}

/// `TVPIsExistentStorage`: the mounted storage first, then the auto search
/// paths.
fn is_existent_storage(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Try the raw name first (absolute paths with mixed case), then the
    // normalized (lowercased) form.
    if exists_in_storage(name) || exists_in_storage(&normalize_storage_name(name)) {
        return true;
    }
    let normalized = normalize_storage_name(name);
    let base = extract_storage_name(&normalized);
    engine::storage::auto_paths()
        .iter()
        .any(|entry| auto_path_resolves(entry, &base))
}

/// `TVPGetPlacedPath`: the normalized storage name when found (mounted
/// storage or auto paths), `""` when not found. Positive results are cached
/// like the reference's `TVPAutoPathCache`; the cache is cleared by
/// `Storages.clearArchiveCache()` and on any auto-path change.
fn placed_path(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    if let Some(hit) = PLACED_PATH_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(name)
        .cloned()
    {
        return hit;
    }
    let found = compute_placed_path(name);
    if !found.is_empty() {
        PLACED_PATH_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.to_string(), found.clone());
    }
    found
}

/// Uncached placement computation behind [`placed_path`].
fn compute_placed_path(name: &str) -> String {
    let normalized = normalize_storage_name(name);
    if exists_in_storage(&normalized) {
        return normalized;
    }
    // Auto paths: like the reference's auto-path table, the base storage
    // name is looked up inside each auto path dir; when several paths
    // contain the same name the last one wins (the reference's hash-table
    // `Add` overwrites earlier entries).
    let base = extract_storage_name(&normalized);
    let mut found: Option<String> = None;
    for entry in engine::storage::auto_paths() {
        if auto_path_resolves(&entry, &base) {
            found = Some(format!("{entry}{base}"));
        }
    }
    found.unwrap_or_default()
}

/// `Storages.getLocalName`: the absolute local disk path for a disk file, or
/// the name unchanged when no disk file exists.
fn local_name(name: &str) -> Result<String, String> {
    if name.contains('>') {
        return Err(format!(
            "Storages.getLocalName: \"{name}\" is inside an archive and has no local name (the reference throws TVPCannotGetLocalName)"
        ));
    }
    let Some(storage) = storage_arc() else {
        return Ok(name.to_string());
    };
    let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
    let normalized = normalize_storage_name(name);
    let disk = disk_entries(&game_dir);
    if let Some((_, _, path)) = disk
        .into_iter()
        .find(|(n, is_dir, _)| !is_dir && n == &normalized)
    {
        return Ok(path.to_string_lossy().into_owned());
    }
    // Not a disk file: return the name unchanged. (The reference walks the
    // path components against the real filesystem and would produce a
    // meaningless "/name" here, so we keep the input.)
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// stat / fstat
// ---------------------------------------------------------------------------

/// Resolve metadata using the same normalized-name fallback as the other
/// Storages queries. The second lookup matters for callers passing a mixed
/// case storage name on a case-sensitive host filesystem.
fn storage_stat(name: &str) -> Option<StorageMetadata> {
    let storage = storage_arc()?;
    let storage = storage.lock().ok()?;
    storage
        .stat(name)
        .or_else(|| storage.stat(&normalize_storage_name(name)))
}

/// Convert a host timestamp to the seconds representation used by TVP_stat
/// and by the fstat plugin's Date.setTime call.
fn unix_seconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs())
            .ok()
            .map(|seconds| -seconds),
    }
}

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

/// Build a real TJS dictionary and return it through the retained-value ABI.
/// `tjs2_eval` leaves the newly-created object in the engine's object-result
/// slot; retaining that slot is the same pattern used by KAGParser and the
/// visual natives.
fn set_stat_result(
    engine: *mut c_void,
    out: *mut Value,
    metadata: &StorageMetadata,
) -> Result<(), String> {
    let expression = std::ffi::CString::new(stat_dictionary_script(metadata))
        .map_err(|_| "Storages.stat: generated dictionary contained NUL".to_string())?;
    let name = c"Storages.stat";
    let mut result = Value {
        ty: VAL_VOID,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    };
    let mut error = ptr::null_mut();
    // SAFETY: the callback's engine and result pointers are valid for this
    // call; the C ABI copies the expression and error as needed.
    let rc = unsafe {
        tjs2_eval(
            engine.cast::<Engine>(),
            expression.as_ptr(),
            name.as_ptr(),
            &mut result,
            &mut error,
        )
    };
    if rc != 0 {
        let message = if error.is_null() {
            "failed to construct metadata dictionary".to_string()
        } else {
            // SAFETY: tjs2_eval returns a NUL-terminated owned error string.
            let message = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: ownership of the error string belongs to this caller.
            unsafe { tjs2_free_string(error) };
            message
        };
        return Err(message);
    }
    let object = Value {
        ty: tjs2_sys::VAL_OBJECT,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    };
    // SAFETY: result's object is the engine's most recent object result.
    let retained = unsafe { tjs2_retain_value(engine.cast::<Engine>(), &object) };
    if retained.is_null() {
        return Err("Storages.stat: failed to retain metadata dictionary".into());
    }
    // SAFETY: `out` is the trampoline's valid result slot. The C++ side
    // consumes the retained id while converting this result.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = retained as usize;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// getFileList
// ---------------------------------------------------------------------------

/// Windows `FILE_ATTRIBUTE_DIRECTORY` (0x10).
pub const ATTR_DIRECTORY: i64 = 0x10;
/// Windows `FILE_ATTRIBUTE_ARCHIVE` (0x20) — the "normal file" bit.
pub const ATTR_NORMAL: i64 = 0x20;

/// `Storages.getFileList(mask, attr)`: storage names matching a wildcard
/// mask.
///
/// Semantics implemented (documented; the reference in this repo has no
/// `getFileList`, so this follows the migration spec):
///
/// - The mask is normalized (lowercased, `\` → `/`).
/// - If it contains `>` it addresses archives: `arcmask>inarcmask`, where
///   `arcmask` wildcard-matches the archive file name (e.g. `*.xp3>data/*.ks`).
/// - The part before the **last** `/` is a required *literal* directory
///   prefix of the relative storage path (it may be empty — then files at
///   any depth are candidates). The part after it is a `*`/`?` wildcard
///   matched case-insensitively against the **base name** (an empty pattern
///   matches everything, so a mask ending in `/` lists the whole subtree).
/// - Disk files are walked recursively under the game dir and emitted as
///   normalized relative storage names; archive entries are emitted as
///   `arc.xp3>path`.
/// - `attr` filters the results: `0` (omitted) or `0x20` → regular files;
///   `0x10` → directories (emitted with a trailing `/`); bits OR together.
/// - The result is sorted and deduped, and returned as a TJS `Array`.
///
/// The native wrapper marshals these names as a `VAL_ARRAY` (the same path
/// `CSVParser.getNextLine` uses).
fn get_file_list(mask: &str, attr: i64) -> Vec<String> {
    let mask = normalize_storage_name(mask);
    let (arc_pat, in_arc_mask) = match mask.split_once('>') {
        Some((arc, rest)) => (Some(arc.to_string()), rest.to_string()),
        None => (None, mask),
    };
    let (dir_prefix, base_pat) = match in_arc_mask.rsplit_once('/') {
        Some((dir, base)) => (
            format!("{dir}/"),
            if base.is_empty() {
                "*".to_string()
            } else {
                base.to_string()
            },
        ),
        None => (String::new(), in_arc_mask.clone()),
    };
    let include_files = attr == 0 || attr & ATTR_NORMAL != 0;
    let include_dirs = attr & ATTR_DIRECTORY != 0;

    let mut found = BTreeSet::new();

    let Some(storage) = storage_arc() else {
        return Vec::new();
    };

    // Disk files under the game dir, recursively.
    {
        let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
        for (name, is_dir, _) in disk_entries(&game_dir) {
            let base = name.rsplit('/').next().unwrap_or(&name);
            let prefix_ok = dir_prefix.is_empty() || name.starts_with(&dir_prefix);
            let attr_ok = (is_dir && include_dirs) || (!is_dir && include_files);
            if prefix_ok && attr_ok && wildcard_match(&base_pat, base) {
                found.insert(if is_dir { format!("{name}/") } else { name });
            }
        }
    }

    // Archive entries, addressed as "arc.xp3>path".
    {
        let storage = storage.lock().unwrap();
        for (arc_path, arc) in storage.archives() {
            let Some(arc_file) = arc_path.file_name() else {
                continue;
            };
            let arc_file = arc_file.to_string_lossy();
            if let Some(pat) = &arc_pat
                && !wildcard_match(pat, &arc_file)
            {
                continue;
            }
            for entry in arc.entries() {
                let base = entry.name.rsplit('/').next().unwrap_or(&entry.name);
                let prefix_ok = dir_prefix.is_empty() || entry.name.starts_with(&dir_prefix);
                if prefix_ok && wildcard_match(&base_pat, base) {
                    found.insert(format!("{arc_file}>{}", entry.name));
                }
            }
        }
    }

    found.into_iter().collect()
}

// ---------------------------------------------------------------------------
// addAutoPath / removeAutoPath
// ---------------------------------------------------------------------------

fn add_auto_path(path: &str) -> Result<(), String> {
    if !path.ends_with(['/', '\\', '>']) {
        return Err(format!(
            "Storages.addAutoPath: path must end with '/', '\\\\' or '>' (the reference throws TVPMissingPathDelimiterAtLast); got \"{path}\""
        ));
    }
    let entry = normalize_storage_name(path);
    if entry.trim_end_matches(['/', '>']).is_empty() {
        return Err("Storages.addAutoPath: empty path".into());
    }
    engine::storage::add_auto_path(entry);
    clear_placed_path_cache();
    log::debug!("tvp-storages: added auto path \"{path}\"");
    Ok(())
}

fn remove_auto_path(path: &str) -> Result<(), String> {
    if !path.ends_with(['/', '\\', '>']) {
        return Err(format!(
            "Storages.removeAutoPath: path must end with '/', '\\\\' or '>' (the reference throws TVPMissingPathDelimiterAtLast); got \"{path}\""
        ));
    }
    let entry = normalize_storage_name(path);
    engine::storage::remove_auto_path(&entry);
    clear_placed_path_cache();
    log::debug!("tvp-storages: removed auto path \"{path}\"");
    Ok(())
}

// ---------------------------------------------------------------------------
// Pure string helpers (ported verbatim from the reference)
// ---------------------------------------------------------------------------

/// `TVPExtractStorageExt`: the extension of the final component **including
/// the dot**, or `""` when there is none before a `/`, `\` or `>` delimiter.
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

/// `TVPExtractStorageName`: the final path component after the last `/`,
/// `\` or `>` delimiter.
fn extract_storage_name(name: &str) -> String {
    match name.rfind(['\\', '/', '>']) {
        Some(i) => name[i + 1..].to_string(),
        None => name.to_string(),
    }
}

/// `TVPExtractStoragePath`: the path part of `name`, including the last
/// `/`, `\` or `>` delimiter.
fn extract_storage_path(name: &str) -> String {
    match name.rfind(['\\', '/', '>']) {
        Some(i) => name[..=i].to_string(),
        None => String::new(),
    }
}

/// `TVPChopStorageExt`: `name` without its extension (the dot is removed),
/// or the whole name when there is no extension before a delimiter.
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

/// `TVPNormalizeStorageName` for the common case — `Storages.getFullPath`.
fn get_full_path(name: &str) -> String {
    normalize_storage_name(name)
}

// ---------------------------------------------------------------------------
// FFI glue
// ---------------------------------------------------------------------------

thread_local! {
    /// Scratch buffer for `out.string`: stays valid until the next callback
    /// on this thread — long enough, since the C++ side copies the string
    /// immediately after the callback returns.
    static STRING_OUT: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn set_string_out(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: `out` is a valid return slot provided by the C++ trampoline.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

fn set_int_out(out: *mut Value, v: i64) {
    // SAFETY: `out` is a valid return slot provided by the C++ trampoline.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

fn set_void_out(out: *mut Value) {
    // SAFETY: `out` is a valid return slot provided by the C++ trampoline.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Build a malloc'd NUL-terminated UTF-8 error message for `*out_error`
/// (the C++ side frees it with `tjs2_free_string`).
fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: `tjs2_malloc` is malloc-compatible; we write a NUL-terminated
    // copy that the C++ trampoline frees after the callback returns.
    unsafe {
        let buf = tjs2_malloc(bytes.len() + 1) as *mut u8;
        if buf.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *buf.add(bytes.len()) = 0;
        buf as *mut c_char
    }
}

fn set_error(out_error: *mut *mut c_char, msg: &str) {
    // SAFETY: `out_error` points at a valid `char*` slot for this call.
    unsafe { *out_error = alloc_error_string(msg) };
}

/// Read the argument slice. Returns an empty slice when `argc <= 0`.
///
/// # Safety
/// `argv` must be valid for `argc` entries for the duration of the call (the
/// C++ trampoline guarantees this).
unsafe fn args<'a>(argc: c_int, argv: *const Value) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: caller upholds the contract.
    unsafe { std::slice::from_raw_parts(argv, argc as usize) }
}

fn arg_str(v: &Value) -> Option<String> {
    if v.ty == VAL_STRING && !v.string.is_null() {
        // SAFETY: the C++ side marshals strings as NUL-terminated UTF-8,
        // valid for the duration of the call.
        Some(
            unsafe { CStr::from_ptr(v.string) }
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    }
}

fn arg_i64(v: &Value) -> Option<i64> {
    match v.ty {
        VAL_INTEGER => Some(v.integer),
        VAL_REAL => Some(v.real as i64),
        _ => None,
    }
}

fn expect_string_arg(args: &[Value], method: &str) -> Result<String, String> {
    args.first()
        .and_then(arg_str)
        .ok_or_else(|| format!("Storages.{method}: expected a string argument"))
}

// ---------------------------------------------------------------------------
// Native methods
// ---------------------------------------------------------------------------

extern "C" fn native_is_existent_storage(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    match expect_string_arg(args, "isExistentStorage") {
        Ok(name) => {
            set_int_out(out, i64::from(is_existent_storage(&name)));
            0
        }
        Err(msg) => {
            set_error(out_error, &msg);
            1
        }
    }
}

extern "C" fn native_get_file_list(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let mask = match expect_string_arg(args, "getFileList") {
        Ok(m) => m,
        Err(msg) => {
            set_error(out_error, &msg);
            return 1;
        }
    };
    let attr = args.get(1).and_then(arg_i64).unwrap_or(0);
    let names = get_file_list(&mask, attr);
    csv_parser::set_array_strings_out(out, &names);
    0
}

extern "C" fn native_add_auto_path(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    match expect_string_arg(args, "addAutoPath").and_then(|p| add_auto_path(&p)) {
        Ok(()) => {
            set_void_out(out);
            0
        }
        Err(msg) => {
            set_error(out_error, &msg);
            1
        }
    }
}

extern "C" fn native_remove_auto_path(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    match expect_string_arg(args, "removeAutoPath").and_then(|p| remove_auto_path(&p)) {
        Ok(()) => {
            set_void_out(out);
            0
        }
        Err(msg) => {
            set_error(out_error, &msg);
            1
        }
    }
}

extern "C" fn native_get_local_name(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    match expect_string_arg(args, "getLocalName").and_then(|n| local_name(&n)) {
        Ok(name) => {
            set_string_out(out, &name);
            0
        }
        Err(msg) => {
            set_error(out_error, &msg);
            1
        }
    }
}

/// Unary string-in/string-out natives (getFullPath, getPlacedPath, the
/// extract/chop helpers).
macro_rules! native_string_unary {
    ($name:ident, $tjs_name:literal, $f:expr) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            // SAFETY: the trampoline guarantees argv/out/out_error validity.
            let args = unsafe { args(argc, argv) };
            match expect_string_arg(args, $tjs_name) {
                Ok(s) => {
                    set_string_out(out, &$f(&s));
                    0
                }
                Err(msg) => {
                    set_error(out_error, &msg);
                    1
                }
            }
        }
    };
}

native_string_unary!(native_get_full_path, "getFullPath", get_full_path);
native_string_unary!(native_get_placed_path, "getPlacedPath", placed_path);
native_string_unary!(
    native_extract_storage_ext,
    "extractStorageExt",
    extract_storage_ext
);
native_string_unary!(
    native_extract_storage_name,
    "extractStorageName",
    extract_storage_name
);
native_string_unary!(
    native_extract_storage_path,
    "extractStoragePath",
    extract_storage_path
);
native_string_unary!(native_chop_storage_ext, "chopStorageExt", chop_storage_ext);

extern "C" fn native_clear_archive_cache(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // Reference `TVPClearArchiveCache` (`StorageIntf.cpp:728`) clears the
    // archive-handle cache; the port's placed-path cache is the reference's
    // `TVPAutoPathCache`, so both are dropped here. The mounted XP3 handles
    // live in `Storage`, so remount the game directory to release and
    // reopen them lazily (a failure keeps the current handles).
    clear_placed_path_cache();
    if let Some(storage) = storage_arc() {
        let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
        match Storage::mount(&game_dir) {
            Ok(fresh) => {
                *storage.lock().unwrap() = fresh;
                log::debug!("tvp-storages: cleared archive cache (remounted {game_dir:?})");
            }
            Err(e) => log::warn!("tvp-storages: clearArchiveCache remount failed: {e}"),
        }
    }
    set_void_out(out);
    0
}

/// Placeholder native for a pending method: raises a clear TJS error.
macro_rules! native_pending {
    ($name:ident, $tjs_name:literal, $why:literal) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            _out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            set_error(
                out_error,
                concat!("Storages.", $tjs_name, " is not implemented yet: ", $why),
            );
            1
        }
    };
}

/// `Storages.stat(name)` / the fstat plugin's `Storages.fstat(name)`.
///
/// Disk entries include size plus Date-valued mtime/ctime/atime. XP3 entries
/// include their uncompressed size and omit timestamps, matching fstat.dll.
extern "C" fn native_stat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = unsafe { args(argc, argv) };
    let name = match expect_string_arg(args, "stat") {
        Ok(name) => name,
        Err(message) => {
            set_error(out_error, &message);
            return 1;
        }
    };
    let Some(metadata) = storage_stat(&name) else {
        set_error(
            out_error,
            &format!("Storages.stat: storage not found: {name}"),
        );
        return 1;
    };
    match set_stat_result(engine, out, &metadata) {
        Ok(()) => 0,
        Err(message) => {
            set_error(out_error, &message);
            1
        }
    }
}

/// fstat.dll names the same operation `fstat`; keep this alias because game
/// scripts commonly call `Storages.fstat` after linking the plugin.
extern "C" fn native_fstat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    native_stat(engine, argc, argv, out, out_error)
}
native_pending!(
    native_search_cd,
    "searchCD",
    "CD volume search is platform-specific and disabled in the reference"
);

// ---------------------------------------------------------------------------
// Storages.open + the minimal binary stream object
// ---------------------------------------------------------------------------

/// Reference `tTJSBinaryStream` open flags (`tjs.h:243`).
const TJS_BS_READ: i64 = 0;
const TJS_BS_WRITE: i64 = 1;
const TJS_BS_APPEND: i64 = 2;
const TJS_BS_UPDATE: i64 = 3;

/// One `Storages.open` stream. The reference returns a `tTJSBinaryStream`,
/// which has no script-visible Rust counterpart; this minimal native owns the
/// bytes and implements the reference stream operations (`Read`/`Write`/
/// `Seek`/`Close`/`GetSize`/`GetPosition`) so the object is a real, usable
/// stream rather than an opaque token.
struct StorageStreamInst {
    data: Vec<u8>,
    pos: usize,
    /// `TJS_BS_WRITE`/`APPEND`/`UPDATE`: flush back to disk on `close`/drop.
    writable: bool,
    /// Storage name the write-mode flush targets.
    name: String,
}

/// One stream handed from `Storages.open` to the freshly constructed native
/// instance. The VM is single-threaded, so a one-slot handoff is enough: the
/// `create` callback runs synchronously inside `new __TvpStorageStream()`.
static PENDING_STREAM: Mutex<Option<StorageStreamInst>> = Mutex::new(None);

extern "C" fn stream_create(_engine: *mut c_void) -> *mut c_void {
    let inst = PENDING_STREAM
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or(StorageStreamInst {
            data: Vec::new(),
            pos: 0,
            writable: false,
            name: String::new(),
        });
    Box::into_raw(Box::new(inst)) as *mut c_void
}

extern "C" fn stream_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from stream_create's Box::into_raw.
        let inst = unsafe { Box::from_raw(instance as *mut StorageStreamInst) };
        // A writable stream not explicitly closed still flushes on drop.
        if inst.writable && !inst.name.is_empty() {
            flush_stream(&inst);
        }
    }
}

/// Write a writable stream's buffer back to its disk path.
fn flush_stream(inst: &StorageStreamInst) {
    let path = disk_path(&inst.name);
    if let Some(parent) = std::path::Path::new(&path).parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::warn!("Storages.open: cannot create parent of {path}: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&path, &inst.data) {
        log::warn!("Storages.open: write {path} failed: {e}");
    }
}

/// Borrow the native instance payload.
fn stream_mut(instance: *mut c_void) -> &'static mut StorageStreamInst {
    // SAFETY: the dispatcher guarantees `instance` is the payload returned by
    // stream_create for this native object.
    unsafe { &mut *(instance as *mut StorageStreamInst) }
}

/// Retain a byte slice as a real TJS octet result (the C ABI has no direct
/// octet result slot; the C++ side copies the retained octet).
fn set_octet_out(engine: *mut c_void, out: *mut Value, bytes: &[u8]) {
    let ffi = Value {
        ty: tjs2_sys::VAL_OCTET,
        integer: 0,
        real: 0.0,
        string: if bytes.is_empty() {
            ptr::null()
        } else {
            bytes.as_ptr() as *const c_char
        },
        array: ptr::null(),
        array_count: bytes.len() as c_int,
        retained: 0,
    };
    // SAFETY: engine is the live VM and the C++ side copies the bytes into a
    // refcounted octet during the call.
    let id = unsafe { tjs2_retain_value(engine.cast::<Engine>(), &ffi) };
    // SAFETY: `out` is the trampoline's valid result slot; it consumes the id.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// `StorageStream.read(count)` — return up to `count` bytes from the cursor.
extern "C" fn stream_read(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let Some(count) = args.first().and_then(arg_i64) else {
        set_error(out_error, "StorageStream.read requires a byte count");
        return 1;
    };
    let inst = stream_mut(instance);
    let end = inst
        .pos
        .saturating_add(count.max(0) as usize)
        .min(inst.data.len());
    let bytes = inst.data[inst.pos..end].to_vec();
    inst.pos = end;
    set_octet_out(engine, out, &bytes);
    0
}

/// `StorageStream.write(data)` — write an octet (or string bytes) at the
/// cursor, extending the buffer as needed.
extern "C" fn stream_write(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let Some(arg) = args.first() else {
        set_error(out_error, "StorageStream.write requires data");
        return 1;
    };
    // SAFETY: `arg` is a valid value for the duration of the call.
    let bytes = if arg.ty == tjs2_sys::VAL_OCTET {
        unsafe { arg.octet_bytes() }.unwrap_or(&[]).to_vec()
    } else {
        arg_str(arg).unwrap_or_default().into_bytes()
    };
    let inst = stream_mut(instance);
    if !inst.writable {
        set_error(out_error, "StorageStream.write: stream is read-only");
        return 1;
    }
    if inst.pos > inst.data.len() {
        inst.data.resize(inst.pos, 0);
    }
    let end = inst.pos + bytes.len();
    if end > inst.data.len() {
        inst.data.resize(end, 0);
    }
    inst.data[inst.pos..end].copy_from_slice(&bytes);
    inst.pos = end;
    set_int_out(out, bytes.len() as i64);
    0
}

/// `StorageStream.seek(offset, origin)` — reference `Seek` semantics: the
/// cursor never leaves `[0, size]`.
extern "C" fn stream_seek(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let Some(offset) = args.first().and_then(arg_i64) else {
        set_error(out_error, "StorageStream.seek requires an offset");
        return 1;
    };
    let origin = args.get(1).and_then(arg_i64).unwrap_or(0);
    let inst = stream_mut(instance);
    let base = match origin {
        0 => 0i64,
        1 => inst.pos as i64,
        2 => inst.data.len() as i64,
        other => {
            set_error(
                out_error,
                &format!("StorageStream.seek: bad origin {other}"),
            );
            return 1;
        }
    };
    inst.pos = (base + offset).clamp(0, inst.data.len() as i64) as usize;
    set_int_out(out, inst.pos as i64);
    0
}

/// `StorageStream.getSize()`.
extern "C" fn stream_get_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, stream_mut(instance).data.len() as i64);
    0
}

/// `StorageStream.getPosition()`.
extern "C" fn stream_get_position(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, stream_mut(instance).pos as i64);
    0
}

/// `StorageStream.close()` — flush a writable stream to disk.
extern "C" fn stream_close(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = stream_mut(instance);
    if inst.writable && !inst.name.is_empty() {
        flush_stream(inst);
    }
    set_void_out(out);
    0
}

/// `Storages.open(name[, flags])` — open a storage entry and return a binary
/// stream object. Reference flags: `TJS_BS_READ` (0, default), `WRITE` (1),
/// `APPEND` (2), `UPDATE` (3). Read mode must resolve in the mounted storage;
/// write modes start from the existing bytes (when present) and flush to the
/// resolved disk path on `close`.
extern "C" fn native_open(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let name = match expect_string_arg(args, "open") {
        Ok(name) => name,
        Err(message) => {
            set_error(out_error, &message);
            return 1;
        }
    };
    let flags = args.get(1).and_then(arg_i64).unwrap_or(TJS_BS_READ);
    let Some(storage) = storage_arc() else {
        set_error(out_error, "Storages.open: no storage mounted");
        return 1;
    };
    let existing = storage.lock().unwrap().read(&name);
    let (data, pos, writable) = match flags {
        TJS_BS_READ => match existing {
            Ok(data) => (data, 0, false),
            Err(e) => {
                set_error(out_error, &format!("Storages.open: {e}"));
                return 1;
            }
        },
        TJS_BS_WRITE => (Vec::new(), 0, true),
        TJS_BS_APPEND => {
            let data = existing.unwrap_or_default();
            let pos = data.len();
            (data, pos, true)
        }
        TJS_BS_UPDATE => (existing.unwrap_or_default(), 0, true),
        other => {
            set_error(out_error, &format!("Storages.open: unknown flags {other}"));
            return 1;
        }
    };
    *PENDING_STREAM.lock().unwrap_or_else(|p| p.into_inner()) = Some(StorageStreamInst {
        data,
        pos,
        writable,
        name,
    });
    let expression = c"new __TvpStorageStream()";
    let script_name = c"Storages.open";
    let mut result = Value {
        ty: VAL_VOID,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    };
    let mut error = ptr::null_mut();
    // SAFETY: engine is the callback's live VM; the ABI copies the expression.
    let rc = unsafe {
        tjs2_eval(
            engine.cast::<Engine>(),
            expression.as_ptr(),
            script_name.as_ptr(),
            &mut result,
            &mut error,
        )
    };
    if rc != 0 {
        *PENDING_STREAM.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let message = if error.is_null() {
            "failed to construct stream object".to_string()
        } else {
            // SAFETY: tjs2_eval returns a NUL-terminated owned error string.
            let message = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: ownership of the error string belongs to this caller.
            unsafe { tjs2_free_string(error) };
            message
        };
        set_error(out_error, &format!("Storages.open: {message}"));
        return 1;
    }
    let object = Value {
        ty: tjs2_sys::VAL_OBJECT,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    };
    // SAFETY: `result`'s object is the engine's most recent object result.
    let retained = unsafe { tjs2_retain_value(engine.cast::<Engine>(), &object) };
    if retained.is_null() {
        set_error(out_error, "Storages.open: failed to retain stream object");
        return 1;
    }
    // SAFETY: `out` is the trampoline's valid result slot.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = retained as usize;
    }
    0
}

/// Register the minimal `Storages.open` stream class.
fn register_storage_stream(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "__TvpStorageStream",
        create: stream_create,
        destroy: stream_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "read",
                f: stream_read,
            },
            NativeInstanceMethodDef {
                name: "write",
                f: stream_write,
            },
            NativeInstanceMethodDef {
                name: "seek",
                f: stream_seek,
            },
            NativeInstanceMethodDef {
                name: "getSize",
                f: stream_get_size,
            },
            NativeInstanceMethodDef {
                name: "getPosition",
                f: stream_get_position,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: stream_close,
            },
        ],
        properties: vec![],
    })
}

/// `Storages.selectFile(param)` — the built-in GUI file selector.
///
/// This port has no portable headless file dialog, so it behaves like a user
/// cancel in the reference (`TVPSelectFile` returns false): `param.name` is
/// left untouched and the native returns `0`. The game's editor-launch path
/// then falls back to its default editor. This is a documented remaining gap,
/// not a silent success (see `docs/missing/07-text-storage.md`).
extern "C" fn native_select_file(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int_out(out, 0);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `Storages` native class on `engine` (static methods only,
/// matching the current tjs2-sys milestone).
/// Resolve a storage name to a local disk path (the game passes
/// `System.dataPath + name`, i.e. absolute paths under the game dir).
fn disk_path(name: &str) -> String {
    let normalized = normalize_storage_name(name);
    let Some(storage) = storage_arc() else {
        return normalized;
    };
    let game_dir = storage.lock().unwrap().game_dir().to_path_buf();
    let disk = disk_entries(&game_dir);
    for (n, is_dir, path) in disk {
        if !is_dir && n == normalized {
            return path.to_string_lossy().into_owned();
        }
    }
    // Not a mounted disk file: use the name as given (absolute path or
    // game-dir-relative), like the reference's TVPGetLocallyAccessibleName.
    let p = std::path::Path::new(&normalized);
    if p.is_absolute() {
        normalized
    } else {
        game_dir.join(&normalized).to_string_lossy().into_owned()
    }
}

/// `Storages.deleteFile(name)` — remove a disk file (save data cleanup;
/// the reference exposes this via the fstat plugin's Storages patch).
extern "C" fn native_delete_file(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    let name = match expect_string_arg(args, "deleteFile") {
        Ok(n) => n,
        Err(msg) => {
            set_error(out_error, &msg);
            return 1;
        }
    };
    let path = disk_path(&name);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            set_void_out(out);
            0
        }
        Err(e) => {
            // The reference returns false (no exception) when the file is
            // missing; propagate a message otherwise.
            if e.kind() == std::io::ErrorKind::NotFound {
                set_void_out(out);
                0
            } else {
                set_error(out_error, &format!("Storages.deleteFile: {e}"));
                1
            }
        }
    }
}

/// `Storages.copyFile(from, to, failIfExist=false)` — copy a disk file
/// (save-data copy/move; the reference exposes this via fstat's Storages
/// patch: `TVPCopyFile(from, to)` with `failIfExist`).
extern "C" fn native_copy_file(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees argv/out/out_error validity.
    let args = unsafe { args(argc, argv) };
    if args.len() < 2 {
        set_error(out_error, "Storages.copyFile requires 2 arguments");
        return 1;
    }
    let from = match arg_str(&args[0]) {
        Some(n) => n,
        None => {
            set_error(out_error, "Storages.copyFile: from must be a string");
            return 1;
        }
    };
    let to = match arg_str(&args[1]) {
        Some(n) => n,
        None => {
            set_error(out_error, "Storages.copyFile: to must be a string");
            return 1;
        }
    };
    let fail_if_exist = args
        .get(2)
        .map(|v| matches!(v.ty, VAL_INTEGER if v.integer != 0))
        .unwrap_or(false);
    let from_path = disk_path(&from);
    let to_path = disk_path(&to);
    if fail_if_exist && std::path::Path::new(&to_path).exists() {
        set_error(
            out_error,
            &format!("Storages.copyFile: destination already exists: {to}"),
        );
        return 1;
    }
    match std::fs::copy(&from_path, &to_path) {
        Ok(_) => {
            set_void_out(out);
            0
        }
        Err(e) => {
            set_error(out_error, &format!("Storages.copyFile: {e}"));
            1
        }
    }
}

pub fn register_storages(engine: &Tjs2Engine) -> Result<(), String> {
    csv_parser::register_csv_parser(engine)?;
    register_storage_stream(engine)?;
    let builder = NativeClassBuilder {
        name: "Storages",
        properties: Vec::new(),
        methods: vec![
            NativeMethodDef {
                name: "isExistentStorage",
                f: native_is_existent_storage,
            },
            NativeMethodDef {
                name: "getFileList",
                f: native_get_file_list,
            },
            NativeMethodDef {
                name: "addAutoPath",
                f: native_add_auto_path,
            },
            NativeMethodDef {
                name: "removeAutoPath",
                f: native_remove_auto_path,
            },
            NativeMethodDef {
                name: "getLocalName",
                f: native_get_local_name,
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
                name: "stat",
                f: native_stat,
            },
            NativeMethodDef {
                name: "fstat",
                f: native_fstat,
            },
            NativeMethodDef {
                name: "open",
                f: native_open,
            },
            NativeMethodDef {
                name: "searchCD",
                f: native_search_cd,
            },
            NativeMethodDef {
                name: "selectFile",
                f: native_select_file,
            },
            NativeMethodDef {
                name: "copyFile",
                f: native_copy_file,
            },
            NativeMethodDef {
                name: "deleteFile",
                f: native_delete_file,
            },
        ],
    };
    engine.register_native_class(&builder)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_lock {
    /// The TJS2 VM is single-threaded and the storages natives keep a
    /// process-global engine context; parallel tests race on it
    /// (segfault). Serialize with one process-wide lock.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;
    use std::fs;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    /// Unique temp dir, removed on drop (no external test deps).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tvp-storages-test-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Clear the process-global state between tests.
    fn reset_globals() {
        *STORAGE.lock().unwrap() = None;
        engine::storage::set_auto_paths(Vec::new());
    }

    /// Create a game dir with the given files (`rel` → contents; a trailing
    /// `/` creates a directory) and mount it via [`set_storage`].
    fn mount_game(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new("game");
        for (rel, contents) in files {
            let p = dir.path().join(rel);
            if rel.ends_with('/') {
                fs::create_dir_all(p).unwrap();
            } else {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, contents).unwrap();
            }
        }
        let storage = Storage::mount(dir.path()).unwrap();
        set_storage(Some(Arc::new(Mutex::new(storage))));
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    fn engine_with_storages() -> Tjs2Engine {
        let engine = Tjs2Engine::new().unwrap();
        register_storages(&engine).unwrap();
        engine
    }

    fn js_str(s: &str) -> String {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    #[test]
    fn copy_and_delete_file_on_disk() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (dir, path) = mount_game(&[("save01.bmp", "save-data-bytes"), ("savedata/", "")]);
        let engine = engine_with_storages();
        let game = format!("{}/", path.to_string_lossy().replace('\\', "/"));

        // deleteFile on an existing file (absolute game-dir path, like the
        // game's `Storages.deleteFile(DATA_PATH + name)`)
        let r = engine
            .eval(
                &format!(
                    "Storages.deleteFile({})",
                    js_str(&format!("{game}save01.bmp"))
                ),
                "t",
            )
            .unwrap();
        assert_eq!(r, TjsValue::Void);
        assert!(!dir.path().join("save01.bmp").exists());

        // deleteFile on a missing file is not an error (fstat semantics)
        let r = engine
            .eval(
                &format!(
                    "Storages.deleteFile({})",
                    js_str(&format!("{game}nope.bmp"))
                ),
                "t",
            )
            .unwrap();
        assert_eq!(r, TjsValue::Void);

        // copyFile creates a destination file
        fs::write(dir.path().join("src.bmp"), "hello").unwrap();
        let r = engine
            .eval(
                &format!(
                    "Storages.copyFile({}, {}, false)",
                    js_str(&format!("{game}src.bmp")),
                    js_str(&format!("{game}savedata/dst.bmp"))
                ),
                "t",
            )
            .unwrap();
        assert_eq!(r, TjsValue::Void);
        assert_eq!(
            fs::read_to_string(dir.path().join("savedata/dst.bmp")).unwrap(),
            "hello"
        );

        // failIfExist=true refuses to overwrite (raises a TJS error)
        let err = engine
            .eval(
                &format!(
                    "Storages.copyFile({0}, {1}, true)",
                    js_str(&format!("{game}src.bmp")),
                    js_str(&format!("{game}savedata/dst.bmp"))
                ),
                "t",
            )
            .err();
        assert!(err.is_some(), "failIfExist=true should raise");
        // destination still intact
        assert_eq!(
            fs::read_to_string(dir.path().join("savedata/dst.bmp")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn normalize_storage_name_lowercases_and_replaces_backslashes() {
        let _vm_lock = vm_lock();
        assert_eq!(
            normalize_storage_name(r"Data\BG\Title.jpg"),
            "data/bg/title.jpg"
        );
        assert_eq!(normalize_storage_name("Startup.tjs"), "startup.tjs");
        assert_eq!(
            normalize_storage_name("arc.xp3>Data/X.TJS"),
            "arc.xp3>data/x.tjs"
        );
    }

    #[test]
    fn wildcard_match_semantics() {
        let _vm_lock = vm_lock();
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("", ""));
        assert!(!wildcard_match("", "x"));
        assert!(wildcard_match("*.tjs", "a.tjs"));
        assert!(!wildcard_match("*.tjs", "a.txt"));
        assert!(wildcard_match("?x", "ax"));
        assert!(!wildcard_match("?x", "abc"));
        assert!(wildcard_match("a*e", "apple"));
        assert!(wildcard_match("A*", "apple"), "case-insensitive");
        assert!(wildcard_match("a*c*d", "abcd"));
        assert!(wildcard_match("a*b*c", "aXbYc"));
        assert!(!wildcard_match("a*b", "ac"));
        assert!(wildcard_match("*tjs", "a.tjs"));
    }

    #[test]
    fn is_existent_storage_disk_and_mixed_case() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (_dir, _) = mount_game(&[
            ("startup.tjs", "System.init"),
            ("data/a.tjs", "x"),
            ("Data/BG.png", "img"),
        ]);
        let engine = engine_with_storages();

        let exists = |name: &str| {
            engine
                .eval(
                    &format!("Storages.isExistentStorage({})", js_str(name)),
                    "t",
                )
                .unwrap()
        };
        assert_eq!(exists("startup.tjs"), TjsValue::Integer(1));
        assert_eq!(exists("data/a.tjs"), TjsValue::Integer(1));
        // mixed-case disk file: exact case resolves directly, and the
        // normalized name resolves via the case-insensitive fallback
        assert_eq!(exists("Data/BG.png"), TjsValue::Integer(1));
        assert_eq!(exists("data/bg.png"), TjsValue::Integer(1));
        assert_eq!(exists("missing.tjs"), TjsValue::Integer(0));
        assert_eq!(exists(""), TjsValue::Integer(0));
    }

    #[test]
    fn get_file_list_wildcards_and_attrs() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (_dir, _) = mount_game(&[
            ("a.tjs", "1"),
            ("data/b.tjs", "2"),
            ("data/sub/c.tjs", "3"),
            ("pic.png", "4"),
        ]);
        let engine = engine_with_storages();

        let list = |mask: &str, attr: Option<i64>| {
            let call = match attr {
                Some(a) => format!("Storages.getFileList({}, {a})", js_str(mask)),
                None => format!("Storages.getFileList({})", js_str(mask)),
            };
            // The native returns a TJS Array; join it in-script so the test
            // can assert on a plain string result.
            let expr = format!("{call}.join('\\n')");
            match engine.eval(&expr, "t").unwrap() {
                TjsValue::String(s) => s.split('\n').map(str::to_string).collect::<Vec<_>>(),
                other => panic!("expected a string, got {other:?}"),
            }
        };

        // base-name matching, recursive across the whole game dir
        let names = list("*.tjs", None);
        assert!(names.contains(&"a.tjs".to_string()));
        assert!(names.contains(&"data/b.tjs".to_string()));
        assert!(names.contains(&"data/sub/c.tjs".to_string()));
        assert!(!names.contains(&"pic.png".to_string()));

        // a directory prefix restricts the subtree
        let names = list("data/*.tjs", None);
        assert!(!names.contains(&"a.tjs".to_string()));
        assert!(names.contains(&"data/b.tjs".to_string()));
        assert!(names.contains(&"data/sub/c.tjs".to_string()));

        // an exact-name mask still matches at any depth
        let names = list("c.tjs", None);
        assert_eq!(names, vec!["data/sub/c.tjs".to_string()]);

        // attr 0x10 lists directories with a trailing '/'
        let names = list("*", Some(0x10));
        assert!(names.contains(&"data/".to_string()));
        assert!(!names.contains(&"a.tjs".to_string()));

        // attr 0x20 lists normal files (the default behavior)
        let names = list("*.tjs", Some(0x20));
        assert!(names.contains(&"a.tjs".to_string()));

        // The native really returns a TJS Array (not a joined string): it has
        // a `.count` and indexes like any Array.
        assert_eq!(
            engine
                .eval("Storages.getFileList('*.tjs').count", "t")
                .unwrap(),
            TjsValue::Integer(3)
        );
        assert_eq!(
            engine
                .eval("Storages.getFileList('*.tjs')[0]", "t")
                .unwrap(),
            TjsValue::String("a.tjs".into())
        );
    }

    #[test]
    fn csv_parser_decodes_cp932_storage_end_to_end() {
        let _vm_lock = vm_lock();
        reset_globals();
        // `system/charData.csv` in the reference game is Windows-31J (cp932),
        // not UTF-8. Build a minimal two-row CSV with a Japanese name and run
        // it through the same `initStorage`/`getNextLine` flow `CharDataInit`
        // uses.
        let dir = TempDir::new("csv-cp932");
        let mut csv = Vec::new();
        csv.extend_from_slice(b"index,name\n");
        csv.extend_from_slice(b"0,");
        // "キャラ名" in cp932.
        csv.extend_from_slice(&[0x83, 0x4c, 0x83, 0x83, 0x83, 0x89, 0x96, 0xbc]);
        fs::write(dir.path().join("charData.csv"), &csv).unwrap();
        let storage = Storage::mount(dir.path()).unwrap();
        set_storage(Some(Arc::new(Mutex::new(storage))));
        let engine = engine_with_storages();

        let result = engine
            .eval(
                "(function(){ var c = new CSVParser(); c.initStorage('charData.csv'); \
                 var h = c.getNextLine(); var r = c.getNextLine(); \
                 return h[1] + ':' + r[1]; })()",
                "t",
            )
            .unwrap();
        assert_eq!(result, TjsValue::String("name:キャラ名".into()));
    }

    #[test]
    fn auto_path_add_remove_and_lookup() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (_game, _) = mount_game(&[("startup.tjs", "s")]);
        let auto = TempDir::new("auto");
        fs::write(auto.path().join("patch.tjs"), "p").unwrap();
        let engine = engine_with_storages();

        let exists = |name: &str| {
            engine
                .eval(
                    &format!("Storages.isExistentStorage({})", js_str(name)),
                    "t",
                )
                .unwrap()
        };
        assert_eq!(exists("patch.tjs"), TjsValue::Integer(0));

        // the reference requires a trailing delimiter
        let err = engine
            .eval("Storages.addAutoPath('no/trailing/slash')", "t")
            .unwrap_err();
        assert!(err.to_string().contains("trailing"), "unexpected: {err}");

        let add = format!(
            "Storages.addAutoPath({})",
            js_str(&format!("{}/", auto.path().display()))
        );
        engine.eval(&add, "t").unwrap();
        assert_eq!(exists("patch.tjs"), TjsValue::Integer(1));

        // duplicate adds are harmless (deduped)
        engine.eval(&add, "t").unwrap();
        assert_eq!(exists("patch.tjs"), TjsValue::Integer(1));

        let remove = format!(
            "Storages.removeAutoPath({})",
            js_str(&format!("{}/", auto.path().display()))
        );
        engine.eval(&remove, "t").unwrap();
        assert_eq!(exists("patch.tjs"), TjsValue::Integer(0));
    }

    #[test]
    fn get_local_name_disk_and_not_found() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (_dir, _) = mount_game(&[("startup.tjs", "s"), ("Data/BG.png", "img")]);
        let engine = engine_with_storages();

        let TjsValue::String(s) = engine
            .eval("Storages.getLocalName('startup.tjs')", "t")
            .unwrap()
        else {
            panic!("expected a string");
        };
        assert!(s.ends_with("startup.tjs"), "unexpected: {s}");

        // a mixed-case disk file resolves to the actual on-disk path
        let TjsValue::String(s) = engine
            .eval("Storages.getLocalName('data/bg.png')", "t")
            .unwrap()
        else {
            panic!("expected a string");
        };
        assert!(s.ends_with("BG.png"), "unexpected: {s}");

        // not on disk → the name is returned unchanged
        assert_eq!(
            engine
                .eval("Storages.getLocalName('missing.tjs')", "t")
                .unwrap(),
            TjsValue::String("missing.tjs".into())
        );
    }

    #[test]
    fn string_helpers_match_reference() {
        let _vm_lock = vm_lock();
        reset_globals();
        let engine = engine_with_storages();
        let eval = |expr: &str| engine.eval(expr, "t").unwrap();

        assert_eq!(
            eval("Storages.extractStorageName('data/x.tjs')"),
            TjsValue::String("x.tjs".into())
        );
        // the `>` archive delimiter splits like '/' and '\'
        assert_eq!(
            eval("Storages.extractStorageName('arc.xp3>data/x.tjs')"),
            TjsValue::String("x.tjs".into())
        );
        assert_eq!(
            eval("Storages.extractStorageExt('a/b.ks')"),
            TjsValue::String(".ks".into())
        );
        assert_eq!(
            eval("Storages.extractStoragePath('data/x.tjs')"),
            TjsValue::String("data/".into())
        );
        assert_eq!(
            eval("Storages.chopStorageExt('data/x.tjs')"),
            TjsValue::String("data/x".into())
        );
        assert_eq!(
            eval("Storages.getFullPath('Data\\\\X.TJS')"),
            TjsValue::String("data/x.tjs".into())
        );
        assert_eq!(
            eval("Storages.getPlacedPath('missing.tjs')"),
            TjsValue::String(String::new())
        );
        assert_eq!(eval("Storages.clearArchiveCache()"), TjsValue::Void);
    }

    /// Build the smallest valid raw XP3 archive needed by the metadata test.
    fn test_xp3(name: &str, data: &[u8]) -> Vec<u8> {
        let mut archive = vec![
            0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01,
        ];
        archive.extend_from_slice(&0u64.to_le_bytes());
        let segment_start = archive.len() as u64;
        archive.extend_from_slice(data);
        let index_offset = archive.len() as u64;

        let utf16: Vec<u16> = name.encode_utf16().collect();
        let mut info = Vec::new();
        info.extend_from_slice(b"info");
        info.extend_from_slice(&(22u64 + utf16.len() as u64 * 2).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&(data.len() as i64).to_le_bytes());
        info.extend_from_slice(&(data.len() as i64).to_le_bytes());
        info.extend_from_slice(&(utf16.len() as i16).to_le_bytes());
        for unit in utf16 {
            info.extend_from_slice(&unit.to_le_bytes());
        }

        let mut segm = Vec::new();
        segm.extend_from_slice(b"segm");
        segm.extend_from_slice(&28u64.to_le_bytes());
        segm.extend_from_slice(&0u32.to_le_bytes());
        segm.extend_from_slice(&(segment_start as i64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as i64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as i64).to_le_bytes());

        let mut file = Vec::new();
        file.extend_from_slice(b"File");
        file.extend_from_slice(&((info.len() + segm.len()) as u64).to_le_bytes());
        file.extend_from_slice(&info);
        file.extend_from_slice(&segm);

        archive.push(0); // raw index
        archive.extend_from_slice(&(file.len() as u64).to_le_bytes());
        archive.extend_from_slice(&file);
        archive[11..19].copy_from_slice(&index_offset.to_le_bytes());
        archive
    }

    #[test]
    fn stat_archive_entry_reports_uncompressed_size_without_dates() {
        let _vm_lock = vm_lock();
        reset_globals();
        let dir = TempDir::new("archive-stat");
        fs::write(
            dir.path().join("data.xp3"),
            test_xp3("Data/inside.ks", b"archive bytes"),
        )
        .unwrap();
        let storage = Storage::mount(dir.path()).unwrap();
        let metadata = storage
            .stat("DATA.XP3>DATA/INSIDE.KS")
            .expect("archive entry metadata");
        assert_eq!(metadata.size, 13);
        assert!(metadata.modified.is_none());
        assert!(metadata.accessed.is_none());
        assert!(metadata.created.is_none());

        set_storage(Some(Arc::new(Mutex::new(storage))));
        let engine = engine_with_storages();
        assert_eq!(
            engine
                .eval("Storages.fstat('data.xp3>data/inside.ks').size", "t")
                .unwrap(),
            TjsValue::Integer(13)
        );
        assert_eq!(
            engine
                .eval("typeof Storages.stat('data.xp3>data/inside.ks').mtime", "t")
                .unwrap(),
            TjsValue::String("undefined".into())
        );
    }

    #[test]
    fn stat_and_fstat_return_metadata_dictionaries() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (_dir, _path) = mount_game(&[("a.tjs", "12345")]);
        let engine = engine_with_storages();

        assert_eq!(
            engine.eval("Storages.stat('a.tjs').size", "t").unwrap(),
            TjsValue::Integer(5)
        );
        // Disk timestamps are Date objects, not encoded strings or integers.
        assert_eq!(
            engine
                .eval("typeof Storages.stat('a.tjs').mtime", "t")
                .unwrap(),
            TjsValue::String("Object".into())
        );
        assert_eq!(
            engine
                .eval("typeof Storages.stat('a.tjs').atime", "t")
                .unwrap(),
            TjsValue::String("Object".into())
        );
        assert_eq!(
            engine.eval("Storages.fstat('a.tjs').size", "t").unwrap(),
            TjsValue::Integer(5)
        );

        // `selectFile` cannot show a headless dialog, so it degrades like a
        // user cancel (returns false/0) instead of raising.
        assert_eq!(
            engine
                .eval("Storages.selectFile(%[name:'', title:'x'])", "t")
                .unwrap(),
            TjsValue::Integer(0)
        );

        // `open` returns a real binary stream: read advances the cursor,
        // seek rewinds it, and the size reflects the storage bytes.
        engine
            .exec_script(
                "var st = Storages.open('a.tjs'); \
                 var streamType = typeof st; \
                 var size = st.getSize(); \
                 st.read(2); \
                 var pos = st.getPosition(); \
                 st.seek(0, 0); \
                 var rewound = st.getPosition(); \
                 st.close();",
                "t",
            )
            .unwrap();
        assert_eq!(
            engine.eval("streamType", "t").unwrap(),
            TjsValue::String("Object".into())
        );
        assert_eq!(engine.eval("size", "t").unwrap(), TjsValue::Integer(5));
        assert_eq!(engine.eval("pos", "t").unwrap(), TjsValue::Integer(2));
        assert_eq!(engine.eval("rewound", "t").unwrap(), TjsValue::Integer(0));
        // `searchCD` remains platform-specific and intentionally raises.
        let err = engine.eval("Storages.searchCD('LABEL')", "t").unwrap_err();
        assert!(
            err.to_string().contains("not implemented"),
            "expected a 'not implemented' error for searchCD, got: {err}"
        );
    }

    #[test]
    fn open_write_mode_flushes_to_disk_and_clear_archive_cache_runs() {
        let _vm_lock = vm_lock();
        reset_globals();
        let (dir, path) = mount_game(&[]);
        let engine = engine_with_storages();
        // Write mode (flags 1): start empty, write, close -> flush to disk.
        engine
            .exec_script(
                "var st = Storages.open('new.bin', 1); \
                 st.write('hello'); \
                 var pos = st.getPosition(); \
                 st.close();",
                "t",
            )
            .unwrap();
        assert_eq!(engine.eval("pos", "t").unwrap(), TjsValue::Integer(5));
        assert_eq!(std::fs::read(path.join("new.bin")).unwrap(), b"hello");
        // `clearArchiveCache` clears the placed-path cache and remounts the
        // storage without disturbing the mounted game.
        engine
            .exec_script("Storages.clearArchiveCache();", "t")
            .unwrap();
        assert_eq!(
            engine
                .eval("Storages.getPlacedPath('new.bin')", "t")
                .unwrap(),
            TjsValue::String("new.bin".into())
        );
        drop(dir);
    }
}

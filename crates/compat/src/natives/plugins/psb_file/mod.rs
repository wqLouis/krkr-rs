//! `PSBFile` native class — the `psbfile.dll` plugin surface.
//!
//! Behavioral reference: `reference/cpp/plugins/psbfile/` (`main.cpp`,
//! `PSBFile.{h,cpp}`, `PSBValue.{h,cpp}`, `PSBHeader.h`, `PSBEnums.h`,
//! `PSBStreamDecompress.cpp`, `PSBLz4Stream.cpp`, `types/*`). The reference
//! registers a factory that accepts an optional path plus the `root` and
//! `load` members; the class's other accessors are not script-visible because
//! PSB values are converted to native TJS `Array`/`Dictionary` objects when
//! `root` is read.
//!
//! # Registered surface
//!
//! * constructor `PSBFile()` or `PSBFile(path)` — exactly the reference
//!   factory (`main.cpp:119-131`): no arguments make an empty instance, a
//!   single string argument loads immediately (a *storage* failure still
//!   raises, like the reference's `TVPCreateStream`, while a *parse* failure
//!   is ignored exactly like the reference, which discards the
//!   `loadPSBFile` result); every other argument combination raises
//!   `TJS_E_INVALIDPARAM`. The reference factory takes no seed argument
//!   (`main.cpp:124-129`), so neither does the port.
//! * `load(path)` → `bool` — exactly one argument (`main.cpp:51`), a string
//!   loads and returns `true`/`false` (`main.cpp:54-78`), an octet returns
//!   `false` (`main.cpp:70-72`), anything else raises `TJS_E_INVALIDPARAM`
//!   (`main.cpp:74`). Storage failures raise (the reference's
//!   `TVPCreateStream` throws, `core/base/StorageIntf.cpp:1189-1219`);
//!   parse failures return `false` (`main.cpp:56-58`).
//! * property `root` — a **fresh** `Dictionary` per read (reference
//!   `getRoot`, `main.cpp:29-42`). When no file is loaded, or the container's
//!   root value is not a dictionary (`getObjects()` null-casts to an empty
//!   object), the result is an empty `Dictionary`, never an error.
//!
//! # Serialized value shapes (`PSBValue.cpp:166-226`)
//!
//! * `PSBNull` → **void** (`:166`), `PSBBool` → integer 0/1 (`:168`),
//!   integers → integer, floats/doubles → real (`:170-183`), strings →
//!   string (`:199`);
//! * `PSBArray` of `u32` → an Array of **int32** values
//!   (`:185-197`: `static_cast<tjs_int32>(i)`), so entries above
//!   `i32::MAX` wrap to negative;
//! * `PSBResource` → a TJS **octet** (`:201-204`) — octet values cannot
//!   cross the port's member ABI as scalars, so resource members are
//!   installed by a script-side `<% … %>` literal assignment against the
//!   parent object (see [`set_octet_member`]);
//! * nested lists/dictionaries → real TJS `Array`/`Dictionary` objects.
//!
//! # `psb://` media
//!
//! `load` registers every top-level resource into the `psb://` storage
//! media (`PSBMedia`, `main.cpp:60-67` + `PSBMedia.h:6-45`) so scripts can
//! open `psb://…` paths afterwards. The registration goes through the L2
//! [`engine::Storage::register_media`] table, which serves those names before
//! disk and archive resolution; the container name is lower-cased from its
//! first `.` exactly as `PSBMedia::NormalizeDomainName` does
//! (`PSBMedia.cpp:16-22`).
//!
//! # Architecture
//!
//! The class owns its parsed [`container::PsbDocument`] in the native
//! instance payload; nothing lives in a process global. Encoding values for
//! TJS builds real TJS `Dictionary`/`Array` objects through the engine, so a
//! game script sees the same shapes as the reference `toTJSVal` conversions.
#![allow(unsafe_code)]

mod container;
mod decompress;
mod error;

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_STRING, Value,
};

use container::{PsbDocument, PsbValue};
use error::PsbError;

use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::system::resolve;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};
use engine::Storage;

// ---------------------------------------------------------------------------
// Instance payload
// ---------------------------------------------------------------------------

/// The parsed document and the path it was loaded from.
#[derive(Default)]
struct PsbFileInst {
    document: Option<PsbDocument>,
    path: Option<String>,
}

fn instance_mut<'a>(instance: *mut c_void) -> &'a mut PsbFileInst {
    // SAFETY: the trampoline passes the live payload created by `psb_create`.
    unsafe { &mut *(instance as *mut PsbFileInst) }
}

fn instance_ref<'a>(instance: *mut c_void) -> &'a PsbFileInst {
    // SAFETY: the trampoline passes the live payload created by `psb_create`.
    unsafe { &*(instance as *const PsbFileInst) }
}

// ---------------------------------------------------------------------------
// Result marshalling
// ---------------------------------------------------------------------------

/// Write a retained object into `out`, consuming the retention (the C++
/// trampoline copies the value before returning).
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = tjs::VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = std::ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id() as usize;
        }
    }
    std::mem::forget(object);
}

// ---------------------------------------------------------------------------
// TJS value construction
// ---------------------------------------------------------------------------

/// A value ready to be installed on a TJS object or returned.
enum Built {
    Scalar(TjsValue),
    Object(DetachedValue),
    Octet(Vec<u8>),
}

fn eval_object(host: &Tjs2Engine, expression: &str, name: &str) -> Result<DetachedValue, String> {
    match host.eval_retained(expression, name) {
        Ok(RetainedValue::Object(object)) => Ok(object),
        Ok(RetainedValue::Value(value)) => Err(format!(
            "`{expression}` did not return an object ({value:?})"
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn array_new(host: &Tjs2Engine) -> Result<DetachedValue, String> {
    eval_object(host, "[]", "PSBFile.array")
}

fn dictionary_new(host: &Tjs2Engine) -> Result<DetachedValue, String> {
    eval_object(host, "%[]", "PSBFile.dictionary")
}

/// The temporary globals through which an octet-valued member is installed.
///
/// A `VAL_OCTET` cannot cross the port's member ABI as a scalar (the safe
/// `TjsValue` surface has no octet variant and a handle-less `Object` value
/// resolves against the last object result), but a TJS script *can* build a
/// `<% … %>` literal. The parent object is parked in a temporary global, the
/// assignment runs as script text, and the globals are cleared afterwards —
/// so the reference's octet member (`PSBValue.cpp:201-204`) is reproduced
/// without changing the VM surface.
const PSB_BUF_GLOBAL: &str = "__krkr_psb_buf";
const PSB_KEY_GLOBAL: &str = "__krkr_psb_key";

/// Install `bytes` as the octet member `key` on `parent` via a script-side
/// octet literal (see [`PSB_BUF_GLOBAL`]).
fn set_octet_member(
    host: &Tjs2Engine,
    parent: tjs::Tjs2ValueId,
    key: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let global = eval_object(host, "global", "PSBFile.global")?;
    // A duplicate so the caller's own retention of `parent` stays intact;
    // `set_member` consumes the duplicate.
    let holder = host.duplicate_retained(parent)?;
    host.set_member(
        global.raw_id(),
        PSB_BUF_GLOBAL,
        &TjsValue::Retained(holder.raw_id() as u64),
    )?;
    drop(holder);
    host.set_member(
        global.raw_id(),
        PSB_KEY_GLOBAL,
        &TjsValue::String(key.to_string()),
    )?;
    let mut source = String::with_capacity(bytes.len() * 3 + 64);
    source.push_str("global.");
    source.push_str(PSB_BUF_GLOBAL);
    source.push_str("[global.");
    source.push_str(PSB_KEY_GLOBAL);
    source.push_str("] = <% ");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(source, "{byte:02x} ");
    }
    source.push_str("%>;");
    let result = host.exec_script(&source, "PSBFile.resource");
    // Clear the temporaries on every path so no reference to the parent
    // object survives the call.
    let _ = host.set_member(global.raw_id(), PSB_BUF_GLOBAL, &TjsValue::Void);
    let _ = host.set_member(global.raw_id(), PSB_KEY_GLOBAL, &TjsValue::Void);
    result.map(|_| ()).map_err(|error| error.to_string())?;
    Ok(())
}

/// Install `built` as `key` on `parent`.
fn set_built(
    host: &Tjs2Engine,
    parent: tjs::Tjs2ValueId,
    key: &str,
    built: Built,
) -> Result<(), String> {
    match built {
        Built::Scalar(value) => host.set_member(parent, key, &value),
        Built::Object(object) => {
            let id = object.raw_id() as usize as u64;
            host.set_member(parent, key, &TjsValue::Retained(id))?;
            drop(object);
            Ok(())
        }
        Built::Octet(bytes) => set_octet_member(host, parent, key, &bytes),
    }
}

/// Convert a parsed value into a TJS value/object tree.
fn build_value(
    host: &Tjs2Engine,
    document: &PsbDocument,
    value: &PsbValue,
) -> Result<Built, String> {
    match value {
        // Reference `PSBNull::toTJSVal` returns a void variant
        // (`PSBValue.cpp:166`), so the member reads as `void`, not `null`.
        PsbValue::Null => Ok(Built::Scalar(TjsValue::Void)),
        PsbValue::Bool(value) => Ok(Built::Scalar(TjsValue::Integer(i64::from(*value)))),
        PsbValue::Int(value) => Ok(Built::Scalar(TjsValue::Integer(*value))),
        PsbValue::Float(value) => Ok(Built::Scalar(TjsValue::Real(f64::from(*value)))),
        PsbValue::Double(value) => Ok(Built::Scalar(TjsValue::Real(*value))),
        PsbValue::String(value) => Ok(Built::Scalar(TjsValue::String(value.clone()))),
        PsbValue::Resource(reference) => Ok(Built::Octet(
            document
                .resource_bytes(*reference)
                .unwrap_or_default()
                .to_vec(),
        )),
        PsbValue::Array(values) => {
            let array = array_new(host)?;
            for (index, value) in values.iter().enumerate() {
                // Reference `PSBArray::toTJSVal` narrows to `tjs_int32`
                // (`PSBValue.cpp:188`), so entries above `i32::MAX` wrap
                // negative.
                host.set_member(
                    array.raw_id(),
                    &index.to_string(),
                    &TjsValue::Integer(i64::from(*value as i32)),
                )?;
            }
            Ok(Built::Object(array))
        }
        PsbValue::List(items) => {
            let array = array_new(host)?;
            for (index, item) in items.iter().enumerate() {
                let built = build_value(host, document, item)?;
                set_built(host, array.raw_id(), &index.to_string(), built)?;
            }
            Ok(Built::Object(array))
        }
        PsbValue::Dictionary(entries) => {
            let dictionary = dictionary_new(host)?;
            for (key, item) in entries {
                let built = build_value(host, document, item)?;
                set_built(host, dictionary.raw_id(), key, built)?;
            }
            Ok(Built::Object(dictionary))
        }
    }
}

/// Build the `root` dictionary the reference `getRoot` returns
/// (`main.cpp:29-42`).
///
/// The reference only enumerates `getObjects()`, which null-casts for every
/// non-dictionary root and yields an empty custom object; a dictionary root
/// produces a **fresh** object per read.
fn build_root(host: &Tjs2Engine, document: &PsbDocument) -> Result<DetachedValue, String> {
    let PsbValue::Dictionary(entries) = &document.root else {
        return dictionary_new(host);
    };
    let dictionary = dictionary_new(host)?;
    for (key, item) in entries {
        let built = build_value(host, document, item)?;
        set_built(host, dictionary.raw_id(), key, built)?;
    }
    Ok(dictionary)
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn psb_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PsbFileInst::default())) as *mut c_void
}

extern "C" fn psb_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `psb_create` and is released exactly once.
    drop(unsafe { Box::from_raw(instance as *mut PsbFileInst) });
}

extern "C" fn psb_invalidate(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = instance_mut(instance);
    inst.document = None;
    inst.path = None;
}

/// Load `path` through the attached storage and parse it.
///
/// Reference semantics (`psbfile/main.cpp` + `PSBFile.cpp`):
///
/// * a storage-read failure raises — the reference's `TVPCreateStream`
///   throws out of `loadPSBFile` (`core/base/StorageIntf.cpp:1189-1219`,
///   `PSBFile.cpp:531-534`);
/// * a bad signature or `MDF`/`MFL` magic raises — `parsePSBHeader` throws
///   and nothing in `loadPSBFile` catches it (`PSBHeader.h:69-75`,
///   `PSBFile.cpp:546`);
/// * every other parse failure (decompression, encryption, unsupported
///   version, unparsable objects) returns `Ok(false)` with the previous
///   document intact (`PSBFile.cpp:535-551` — `_stream.reset(); return
///   false;`, and `main.cpp:56-58` turns that into `loadSuccess = false`).
fn load_document(ctx: &CompatCtx, inst: &mut PsbFileInst, path: &str) -> Result<bool, String> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return Err("PSBFile: no storage is attached".into());
    };
    // SAFETY: the host keeps the attached storage alive for the binding.
    let bytes = unsafe { &*storage }
        .read(strip_file_media(path))
        .map_err(|error| error.to_string())?;
    match container::parse_stream(&bytes, 0) {
        Ok(document) => {
            inst.document = Some(document);
            inst.path = Some(path.to_string());
            register_media(storage, path, inst.document.as_ref().expect("just stored"));
            Ok(true)
        }
        Err(error @ (PsbError::BadSignature(_) | PsbError::ArchiveNotPsb)) => {
            Err(error.to_string())
        }
        Err(error) => {
            // Reference: "cannot load psb file" / "Can not parse objects" →
            // `false` (`main.cpp:56-58`, `PSBFile.cpp:510-511`).
            log::warn!("PSBFile: cannot load {path}: {error}");
            Ok(false)
        }
    }
}

/// Registers every top-level resource of a loaded PSB under
/// `psb://<normalized path>/<key>` in the L2 storage media table.
///
/// Reference `PSBFile.load` walks `getObjects()` and adds each `PSBResource`
/// to the `psb://` media after `PSBMedia::NormalizeDomainName` lower-cases the
/// container's extension (`reference/cpp/plugins/psbfile/main.cpp:60-68`,
/// `PSBMedia.cpp:16-22`). `PSDInfo.loadPIMG` then opens those entries as
/// `psb://<container>/<layer_id>.tlg` (`data.xp3:system/psdlayer.tjs`).
fn register_media(storage: *mut Storage, path: &str, document: &PsbDocument) {
    let PsbValue::Dictionary(entries) = &document.root else {
        return;
    };
    let container = normalize_psb_domain(path);
    // SAFETY: the caller holds the live storage pointer for the call and the
    // registration only touches its interior-mutable media table.
    let storage = unsafe { &*storage };
    for (key, value) in entries {
        let PsbValue::Resource(reference) = value else {
            continue;
        };
        let Some(bytes) = document.resource_bytes(*reference) else {
            continue;
        };
        storage.register_media(format!("psb://{container}/{key}"), bytes.to_vec());
    }
}

/// Reference `PSBMedia::NormalizeDomainName` (`PSBMedia.cpp:16-22`):
/// lower-case from the first `.` onward.
fn normalize_psb_domain(path: &str) -> String {
    match path.find('.') {
        Some(dot) => format!("{}{}", &path[..dot], path[dot..].to_lowercase()),
        None => path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Constructor and `load`
// ---------------------------------------------------------------------------

/// `new PSBFile()` / `new PSBFile(path)` — reference `PSBFileFactory`
/// (`psbfile/main.cpp:119-131`): zero arguments, or exactly one string;
/// anything else is `TJS_E_INVALIDPARAM`. The reference discards the
/// `loadPSBFile` result in the factory (`main.cpp:127`), so a parse failure
/// leaves an empty instance — only storage/header failures raise.
extern "C" fn psb_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "PSBFile: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if !arguments.is_empty() {
        if arguments.len() != 1 || arguments[0].ty != VAL_STRING {
            return report_error(
                out_error,
                "PSBFile: invalid parameters (expected PSBFile() or PSBFile(path))",
            );
        }
        let path = value_as_string(&arguments[0]);
        let inst = instance_mut(instance);
        if let Err(error) = load_document(&ctx, inst, &path) {
            return report_error(out_error, &format!("PSBFile: {error}"));
        }
    }
    set_void(out);
    0
}

/// `load(path)` — reference `load` (`psbfile/main.cpp:46-79`): exactly one
/// argument (`:51`), a string loads (`:54-68`), an octet returns `false`
/// (`:70-72`), any other type is `TJS_E_INVALIDPARAM` (`:74`).
extern "C" fn psb_load(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "PSBFile.load: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() != 1 {
        return report_error(out_error, "PSBFile.load requires exactly 1 argument");
    }
    let path_value = &arguments[0];
    if path_value.ty == tjs::VAL_OCTET {
        // `LOGGER->critical("PSBFile::load stream no implement!");
        //  loadSuccess = false;` — `main.cpp:70-72`.
        set_bool(out, false);
        return 0;
    }
    if path_value.ty != VAL_STRING {
        return report_error(out_error, "PSBFile.load: invalid parameter type");
    }
    let path = value_as_string(path_value);
    let inst = instance_mut(instance);
    match load_document(&ctx, inst, &path) {
        Ok(result) => set_bool(out, result),
        Err(error) => return report_error(out_error, &format!("PSBFile.load: {error}")),
    }
    0
}

// ---------------------------------------------------------------------------
// Root accessor
// ---------------------------------------------------------------------------

/// `root` — reference `getRoot` (`psbfile/main.cpp:29-42`): a fresh
/// dictionary built from the container's root objects, or an empty
/// dictionary when nothing is loaded (the reference's null `getObjects()`
/// skips the loop but still returns the object, `:33` and `:42`).
extern "C" fn psb_root_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    root_result(engine, instance, out, out_error)
}

fn root_result(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "PSBFile: compat context is not bound");
    };
    let host = resolved.engine();
    let built = match instance_ref(instance).document.as_ref() {
        Some(document) => build_root(host, document),
        None => dictionary_new(host),
    };
    match built {
        Ok(object) => set_retained_out(out, object),
        Err(error) => return report_error(out_error, &format!("PSBFile.root: {error}")),
    }
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `PSBFile` native class on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "PSBFile",
        create: psb_create,
        destroy: psb_destroy,
        invalidate: Some(psb_invalidate),
        methods: vec![
            NativeInstanceMethodDef {
                name: "PSBFile",
                f: psb_ctor,
            },
            NativeInstanceMethodDef {
                name: "load",
                f: psb_load,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "root",
            get: Some(psb_root_get),
            set: None,
        }],
    })
}

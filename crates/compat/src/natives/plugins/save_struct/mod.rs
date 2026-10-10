//! `saveStruct` plugin — the `saveStruct.dll` plugin surface.
//!
//! Behavioral reference: `reference/cpp/plugins/saveStruct.cpp`. The plugin
//! attaches structured-serialization members to the TJS2 builtins `Array`
//! (reference `ArrayAdd`) and `Dictionary` (reference `DictAdd`):
//!
//! | class        | member            | reference behavior                                     |
//! |--------------|-------------------|--------------------------------------------------------|
//! | `Array`      | `save2`           | write every element's string form + a newline          |
//! | `Array`      | `saveStruct2`     | write the `saveStruct` form to a file                  |
//! | `Array`      | `toStructString`  | return the `saveStruct` form as a string               |
//! | `Dictionary` | `saveStruct2`     | write the `saveStruct` form to a file                  |
//! | `Dictionary` | `toStructString`  | return the `saveStruct` form as a string               |
//!
//! The reference attaches these with `NCB_ATTACH_CLASS`; this port uses the
//! objthis-only instance-member API
//! ([`CompatCtx::register_instance_members`](crate::CompatCtx::register_instance_members))
//! so the callbacks receive the array/dictionary they were invoked on.
//!
//! # The serialized form
//!
//! [`SERIALIZER_SCRIPT`] reproduces the reference's `getVariantString` /
//! `getArrayString` / `getDictString` exactly:
//!
//! * `void` → `void`, TJS `null` → `nullptr`;
//! * integer → `int N`, real → `real N`, string → a double-quoted,
//!   `"`/`\`-escaped literal, octet → `<% xx xx %>`;
//! * an `Array` instance → `[v0,v1,...]` (comma-separated, no newline);
//! * any other object → `%["key"=>value,...]`, entries separated by `,` and
//!   the configured newline (CRLF unless the newline flag is non-zero).
//!
//! The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which
//! the Rust ABI does not expose. The serializer instead asks the builtin
//! `Array.assign` to copy the dictionary's non-hidden members into an array
//! as `[key, value, key, value, ...]`; `Array.assign` performs exactly the
//! reference's `EnumMembers` walk, so the member order and hidden-member
//! filtering match. Nested objects recurse through the same script.
//!
//! No serialization crate applies: the `saveStruct` format is the
//! reference's own text layout (not JSON/TOML — `int N`, `real N`,
//! `%["k"=>v]`, `<% xx %>`), and member enumeration must run through the
//! VM (`Array.assign`), so the serializer stays the reference's TJS script.
//!
//! # Files
//!
//! `save2`/`saveStruct2` resolve a relative name against the attached L2
//! storage's game directory (an absolute name is used as-is) and write the
//! raw `tjs_char` stream the reference's `tTVPStringStream` produces: UTF-16LE
//! with no BOM or crypt container.

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use tjs::{NativeInstanceMembers, NativeObjThisMethodDef, RetainedValue, TjsValue, Value};

use super::super::system::{resolve, set_string_out, value_as_i64};
use crate::native_util::{args, report_error, set_void, value_as_string};
use crate::{CompatCtx, CompatError};

/// Name of the per-engine helper function the serializer is installed under.
const SERIALIZER_NAME: &str = "__krkr_save_struct_serialize";

/// The reference `getVariantString` / `getArrayString` / `getDictString`,
/// expressed as a TJS function `(obj, newline) -> string`.
///
/// `newline` follows the reference's `tTVPStringStream`: `0` selects CRLF and
/// any non-zero value selects LF. Dictionary entries are separated by `,`
/// plus that newline; array elements are always comma-separated.
/// The helpers are installed as **global** functions rather than nested
/// declarations: the port's TJS2 has no lexical capture for nested functions
/// (a nested `function` is not visible to its siblings), so mutual recursion
/// goes through the global scope.
///
/// `__krkr_save_struct_is_array` guards `instanceof` with `try`/`catch`: the
/// VM raises `TJS_E_NOTIMPL` when `instanceof` is applied to a function (or
/// another non-class object), while the reference's `IsInstanceOf` simply
/// reports false.
///
/// `__krkr_save_struct_dict` skips the members an empty `%[]` already has:
/// the port attaches non-static instance members to the builtin
/// `Dictionary` (the `scripts_ex` helpers and this plugin's own members), so
/// every `Dictionary` instance inherits them and `EnumMembers` reports them.
/// The reference attaches its own `DictAdd` members as `TJS_STATICMEMBER` and
/// other plugins to `Scripts`, so its `getDictString` never sees them; the
/// filter reproduces that observable output without recursing into the
/// attached methods.
const SERIALIZER_SCRIPT: &str = r#"
function __krkr_save_struct_quote(s) {
    if (s === void || s === null) s = "";
    var r = "";
    var n = s.length;
    var i = 0;
    while (i < n) {
        var c = s.charAt(i);
        if (c == '"') {
            r = r + '\\"';
        } else if (c == '\\') {
            r = r + '\\\\';
        } else {
            r = r + c;
        }
        i = i + 1;
    }
    return '"' + r + '"';
}

function __krkr_save_struct_quote_octet(o) {
    var hex = "0123456789abcdef";
    var r = "<% ";
    var n = o.length;
    var i = 0;
    while (i < n) {
        var b = o[i];
        r = r + hex.charAt((b >> 4) & 15) + hex.charAt(b & 15) + " ";
        i = i + 1;
    }
    return r + "%>";
}

function __krkr_save_struct_is_array(v) {
    try {
        return v instanceof "Array";
    } catch (e) {
        return false;
    }
}

function __krkr_save_struct_variant(v, nl) {
    var t = typeof v;
    if (t == "Void") return "void";
    if (t == "Integer") return "int " + v;
    if (t == "Real") return "real " + v;
    if (t == "String") return __krkr_save_struct_quote(v);
    if (t == "Octet") return __krkr_save_struct_quote_octet(v);
    if (t == "Object") {
        if (v === null) return "nullptr";
        if (__krkr_save_struct_is_array(v)) return __krkr_save_struct_arr(v, nl);
        return __krkr_save_struct_dict(v, nl);
    }
    return "void";
}

function __krkr_save_struct_arr(a, nl) {
    var r = "[";
    var count = a.count;
    var i = 0;
    while (i < count) {
        if (i != 0) r = r + ",";
        r = r + __krkr_save_struct_variant(a[i], nl);
        i = i + 1;
    }
    return r + "]";
}

function __krkr_save_struct_dict(d, nl) {
    var NL = (nl != 0) ? "\n" : "\r\n";
    var classpairs = [];
    classpairs.assign(%[]);
    var pairs = [];
    pairs.assign(d);
    var r = "%[";
    var first = true;
    var i = 0;
    while (i < pairs.count) {
        var key = pairs[i];
        var isclass = false;
        var j = 0;
        while (j < classpairs.count) {
            if (classpairs[j] == key) { isclass = true; break; }
            j = j + 2;
        }
        if (!isclass) {
            if (!first) r = r + "," + NL;
            first = false;
            r = r + __krkr_save_struct_quote(key) + "=>" + __krkr_save_struct_variant(pairs[i + 1], nl);
        }
        i = i + 2;
    }
    return r + "]";
}

function __krkr_save_struct_serialize(obj, nl) {
    return __krkr_save_struct_variant(obj, nl);
}
"#;

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Retain `objthis` and run the installed serializer on it.
///
/// The retained object is consumed by the argument conversion (the C++ side
/// copies the retained variant into the call frame), so the local handle's
/// drop afterwards is a safe no-op.
fn serialize_object(
    engine: *mut c_void,
    objthis: *mut c_void,
    newline: i64,
) -> Result<String, String> {
    let Some(resolved) = resolve(engine) else {
        return Err("compat context is not bound".to_string());
    };
    let host = resolved.engine();
    let object = host
        .retain_object_detached(objthis)
        .map_err(|error| error.to_string())?;
    let function = match host
        .eval_retained(SERIALIZER_NAME, "saveStruct")
        .map_err(|error| error.to_string())?
    {
        RetainedValue::Object(function) => function,
        RetainedValue::Value(value) => {
            return Err(format!("serializer is not callable: {value:?}"));
        }
    };
    let result = host.call_detached(
        &function,
        &[
            TjsValue::Retained(object.raw_id() as u64),
            TjsValue::Integer(newline),
        ],
    )?;
    match result {
        TjsValue::String(text) => Ok(text),
        other => Err(format!("serializer returned {other:?}, expected a string")),
    }
}

// ---------------------------------------------------------------------------
// Output paths
// ---------------------------------------------------------------------------

/// Whether `name` is a Windows drive-letter path (`C:/...`), which the
/// reference treats as absolute.
fn is_drive_path(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Resolve a reference stream name to a filesystem path.
///
/// Absolute names (POSIX or a drive letter) are used verbatim; a relative
/// name is joined onto the attached L2 storage's game directory, matching the
/// reference's data-dir-relative `TVPCreateStream`. With no storage attached
/// the name is used as given.
fn output_path(ctx: &CompatCtx, filename: &str) -> PathBuf {
    let path = Path::new(filename);
    if path.is_absolute() || is_drive_path(filename) {
        return path.to_path_buf();
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    if let Some(pointer) = services.storage() {
        // SAFETY: the host keeps the attached storage alive for the binding.
        return unsafe { &*pointer }.game_dir().join(path);
    }
    path.to_path_buf()
}

/// Write the serialized text as the raw `tjs_char` stream the reference's
/// `tTVPStringStream` emits: UTF-16LE with no BOM.
fn write_struct_file(ctx: &CompatCtx, filename: &str, content: &str) -> Result<(), String> {
    let path = output_path(ctx, filename);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let mut bytes = Vec::with_capacity(content.len() * 2);
    for unit in content.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The reference newline selector: `_newline` is LF when the flag is non-zero
/// and CRLF otherwise.
fn newline_for(flag: i64) -> &'static str {
    if flag != 0 { "\n" } else { "\r\n" }
}

// ---------------------------------------------------------------------------
// Array members (`ArrayAdd`)
// ---------------------------------------------------------------------------

/// `Array.save2(filename [, utf [, newline]])` — reference `ArrayAdd::save2`.
///
/// Writes each element's string form followed by the configured newline. The
/// reference reads every element through `tTJSVariant::GetString`, which only
/// accepts strings, so a non-string element is an error.
extern "C" fn array_save2(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "Array.save2 requires 1 argument");
    };
    let filename = value_as_string(filename);
    // The reference ignores the `utf` flag (index 1) and reads the newline
    // selector from index 2.
    let newline = newline_for(argv.get(2).map(value_as_i64).unwrap_or(0));
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Array.save2: compat context is not bound");
    };
    let host = resolved.engine();
    let object = match host.retain_object_detached(objthis) {
        Ok(object) => object,
        Err(error) => return report_error(out_error, &format!("Array.save2: {error}")),
    };
    let count = match host.get_member(object.raw_id(), "count") {
        Ok(TjsValue::Integer(count)) => count,
        Ok(other) => {
            return report_error(
                out_error,
                &format!("Array.save2: Array.count is not an integer ({other:?})"),
            );
        }
        Err(error) => return report_error(out_error, &format!("Array.save2: {error}")),
    };
    let mut content = String::new();
    for index in 0..count {
        let value = match host.get_member(object.raw_id(), &index.to_string()) {
            Ok(value) => value,
            Err(error) => {
                return report_error(out_error, &format!("Array.save2: element {index}: {error}"));
            }
        };
        match value {
            TjsValue::String(text) => content.push_str(&text),
            other => {
                return report_error(
                    out_error,
                    &format!("Array.save2: element {index} is not a string ({other:?})"),
                );
            }
        }
        content.push_str(newline);
    }
    let ctx = resolved.ctx();
    if let Err(error) = write_struct_file(&ctx, &filename, &content) {
        return report_error(out_error, &format!("Array.save2: {error}"));
    }
    set_void(out);
    0
}

/// `Array.saveStruct2(filename [, utf [, newline]])` — reference
/// `ArrayAdd::saveStruct2`.
extern "C" fn array_save_struct2(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "Array.saveStruct2 requires 1 argument");
    };
    let filename = value_as_string(filename);
    let newline = argv.get(2).map(value_as_i64).unwrap_or(0);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Array.saveStruct2: compat context is not bound");
    };
    let content = match serialize_object(engine, objthis, newline) {
        Ok(content) => content,
        Err(error) => return report_error(out_error, &format!("Array.saveStruct2: {error}")),
    };
    let ctx = resolved.ctx();
    if let Err(error) = write_struct_file(&ctx, &filename, &content) {
        return report_error(out_error, &format!("Array.saveStruct2: {error}"));
    }
    set_void(out);
    0
}

/// `Array.toStructString([newline])` — reference `ArrayAdd::toStructString`.
extern "C" fn array_to_struct_string(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let newline = argv.first().map(value_as_i64).unwrap_or(0);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Array.toStructString: compat context is not bound",
        );
    };
    let content = match serialize_object(engine, objthis, newline) {
        Ok(content) => content,
        Err(error) => return report_error(out_error, &format!("Array.toStructString: {error}")),
    };
    let ctx = resolved.ctx();
    set_string_out(&ctx, out, &content);
    0
}

// ---------------------------------------------------------------------------
// Dictionary members (`DictAdd`)
// ---------------------------------------------------------------------------

/// `Dictionary.saveStruct2(filename [, utf [, newline]])` — reference
/// `DictAdd::saveStruct2`.
extern "C" fn dict_save_struct2(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "Dictionary.saveStruct2 requires 1 argument");
    };
    let filename = value_as_string(filename);
    let newline = argv.get(2).map(value_as_i64).unwrap_or(0);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Dictionary.saveStruct2: compat context is not bound",
        );
    };
    let content = match serialize_object(engine, objthis, newline) {
        Ok(content) => content,
        Err(error) => {
            return report_error(out_error, &format!("Dictionary.saveStruct2: {error}"));
        }
    };
    let ctx = resolved.ctx();
    if let Err(error) = write_struct_file(&ctx, &filename, &content) {
        return report_error(out_error, &format!("Dictionary.saveStruct2: {error}"));
    }
    set_void(out);
    0
}

/// `Dictionary.toStructString([newline])` — reference
/// `DictAdd::toStructString`.
extern "C" fn dict_to_struct_string(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let newline = argv.first().map(value_as_i64).unwrap_or(0);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Dictionary.toStructString: compat context is not bound",
        );
    };
    let content = match serialize_object(engine, objthis, newline) {
        Ok(content) => content,
        Err(error) => {
            return report_error(out_error, &format!("Dictionary.toStructString: {error}"));
        }
    };
    let ctx = resolved.ctx();
    set_string_out(&ctx, out, &content);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // Install the serializer helper once per engine. It is a plain global
    // function so the callbacks can resolve it by name; no native payload or
    // process-global state is involved.
    // SAFETY: `ctx` is only created for a bound, live engine.
    unsafe { ctx.engine() }
        .exec_script(SERIALIZER_SCRIPT, "saveStruct")
        .map_err(|error| {
            CompatError::Register(format!("saveStruct: could not install serializer: {error}"))
        })?;

    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Array",
        methods: vec![
            NativeObjThisMethodDef {
                name: "save2",
                f: array_save2,
            },
            NativeObjThisMethodDef {
                name: "saveStruct2",
                f: array_save_struct2,
            },
            NativeObjThisMethodDef {
                name: "toStructString",
                f: array_to_struct_string,
            },
        ],
        properties: vec![],
    })?;

    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Dictionary",
        methods: vec![
            NativeObjThisMethodDef {
                name: "saveStruct2",
                f: dict_save_struct2,
            },
            NativeObjThisMethodDef {
                name: "toStructString",
                f: dict_to_struct_string,
            },
        ],
        properties: vec![],
    })?;

    Ok(())
}

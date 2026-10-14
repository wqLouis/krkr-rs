// Guard self-test near-miss: a plain static member that never invokes a
// script callback. It needs no caller `objthis`, so the `callback-objthis`
// rule must trip nothing here.

use tjs::{NativeMethodDef, TjsValue, Value};

fn scripts_get_md5_hash_string(
    arguments: &[Value],
) -> Result<TjsValue, String> {
    let _ = arguments;
    Ok(TjsValue::String(String::new()))
}

extern "C" fn scripts_get_md5_hash_string_native(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline passes `argc` valid arguments.
    let arguments = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let result = scripts_get_md5_hash_string(arguments)?;
    let _ = (out, out_error, result);
    0
}

fn static_methods() -> Vec<NativeMethodDef> {
    vec![NativeMethodDef {
        name: "getMD5HashString",
        f: scripts_get_md5_hash_string_native,
    }]
}

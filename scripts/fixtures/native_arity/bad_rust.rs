// Fixture for the native-arity self-test: `Layer.setClip` rejects every
// `argc < 4`, including the zero-argument reset form the reference accepts.
use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceMethodDef, Value};

extern "C" fn bad_set_clip(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 4 {
        return report_error(out_error, "Layer.setClip requires 4 arguments");
    }
    set_void(out);
    0
}

fn layer_methods() -> Vec<NativeInstanceMethodDef> {
    vec![NativeInstanceMethodDef {
        name: "setClip",
        f: bad_set_clip,
    }]
}

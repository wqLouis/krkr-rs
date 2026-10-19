// Fixture for the native-arity self-test: `Layer.setClip` handles the
// zero-argument reset form before the arity guard, so it must not be flagged.
use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceMethodDef, Value};

extern "C" fn good_set_clip(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc == 0 {
        reset_clip();
        set_void(out);
        return 0;
    }
    if argc < 4 {
        return report_error(out_error, "Layer.setClip requires 4 arguments");
    }
    set_void(out);
    0
}

fn layer_methods() -> Vec<NativeInstanceMethodDef> {
    vec![NativeInstanceMethodDef {
        name: "setClip",
        f: good_set_clip,
    }]
}

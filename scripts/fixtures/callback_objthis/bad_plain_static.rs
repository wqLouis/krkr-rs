// Guard self-test fixture: the `callback-objthis` rule MUST flag the
// `foreach` member below. Not compiled and not part of any crate.
//
// The shape being caught: the member receives a script callback, invokes it,
// and is registered as a plain static `NativeMethodDef`, whose callback
// signature carries no `objthis`. An anonymous callback then runs with the
// function object as `this`.

use tjs::{DetachedValue, NativeMethodDef, Tjs2Engine, TjsValue, Value};

fn invoke_callback(engine: &Tjs2Engine, callback: &DetachedValue) -> Result<(), String> {
    engine.call_detached(callback, &[TjsValue::Integer(1)])?;
    Ok(())
}

extern "C" fn scripts_foreach(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline passes a live engine and `argc` valid arguments.
    let engine = unsafe { &*(engine as *const Tjs2Engine) };
    let arguments = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let callback = engine.retain_object_arg(&arguments[1])?;
    let _ = invoke_callback(engine, &callback);
    let _ = (out, out_error);
    0
}

fn static_methods() -> Vec<NativeMethodDef> {
    vec![NativeMethodDef {
        name: "foreach",
        f: scripts_foreach,
    }]
}

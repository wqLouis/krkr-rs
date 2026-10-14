// Guard self-test near-miss: the same callback-invoking member as the bad
// fixture, but registered through the objthis dispatch (a static
// `NativeObjThisMethodDef`). The member receives the caller's `objthis` and
// the `callback-objthis` rule must trip nothing here.

use tjs::{DetachedValue, NativeObjThisMethodDef, Tjs2Engine, TjsValue, Value};

fn invoke_callback(
    engine: &Tjs2Engine,
    callback: &DetachedValue,
    objthis: *mut c_void,
) -> Result<(), String> {
    // SAFETY: `objthis` is the receiver the VM passed to this callback.
    unsafe {
        engine.call_detached_with_objthis(callback, &[TjsValue::Integer(1)], objthis)?;
    }
    Ok(())
}

extern "C" fn scripts_foreach(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the trampoline passes a live engine and `argc` valid arguments.
    let engine = unsafe { &*(engine as *const Tjs2Engine) };
    let arguments = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let callback = engine.retain_object_arg(&arguments[1])?;
    let _ = invoke_callback(engine, &callback, objthis);
    let _ = (out, out_error);
    0
}

fn objthis_methods() -> Vec<NativeObjThisMethodDef> {
    vec![NativeObjThisMethodDef {
        name: "foreach",
        f: scripts_foreach,
    }]
}

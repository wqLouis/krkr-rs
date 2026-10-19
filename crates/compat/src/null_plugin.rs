//! The null plugin object (doc 08 §4.2).
//!
//! Linux and Android cannot load the Windows plugin DLLs the games name.
//! `Plugins.link(name)` therefore either activates a built-in emulation or
//! installs a **null plugin object** that accepts every member access, does
//! nothing, logs a warning and **never throws**. This is the single
//! documented exception to the no-silent-fallback rule — and it is not silent.
//!
//! # Mechanism and the ABI limitation
//!
//! `tjs`'s class builder can only register **named** members; there is
//! no wildcard/proxy class. The closest mechanism the ABI supports is the
//! TJS **`missing` member handler**: a native instance class registers a
//! `missing(getorset, name, value)` method and enables `CallMissing` on each
//! instance, after which every absent member resolves to `missing` instead
//! of raising.
//!
//! We register a `NullPlugin` native instance class whose class-name
//! constructor enables `CallMissing` on the object (via
//! `tjs::Tjs2Engine::set_call_missing`) and whose `missing` method logs and
//! returns `true`. [`install`] creates one instance per missing plugin,
//! installs it as a script global under the plugin's stem
//! (`Plugins.link("windowEx.dll")` → `global.windowEx`) and keeps it alive.
//! The install **never replaces an existing binding**: a `global[stem]` that
//! already holds a native class or a script value is kept (a real
//! binding is never clobbered by the null path), the link is still recorded,
//! and [`remove`] then leaves that binding alone. Real plugin emulations
//! register their own classes, so the null object is only a namespace handle
//! for the plugin we could not load.
//!
//! ## What this catches, and what it cannot
//!
//! * **Property reads and writes on the null object never throw.** A read
//!   returns a no-op callable (below); a write is ignored.
//! * **Method calls are intercepted too.** For a get — which is also how the
//!   VM resolves a call to an absent member — we set the property object's
//!   default member (via the `tjs2_prop_set_default` ABI) to a no-op closure,
//!   so `plugin.foo()` runs the no-op instead of raising. A plain read
//!   returns that same no-op closure.
//! * **Unknown class names** (`new SomePluginClass()`) are global lookups,
//!   not member accesses on the null object, so they are not caught.
//!
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void};

use tjs::{Engine, NativeInstanceBuilder, NativeInstanceMethodDef, RetainedValue, TjsValue, Value};

use crate::binding;
use crate::native_util::{args, set_bool, set_void, value_as_string};
use crate::{CompatCtx, CompatError};

/// The VM-visible class name of the null plugin object.
pub const NULL_PLUGIN_CLASS: &str = "NullPlugin";

/// Per-object payload: the plugin name for log messages.
struct NullPluginState {
    /// Single-threaded VM, so a `RefCell` (not a mutex) is enough.
    name: RefCell<String>,
}

extern "C" fn null_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(NullPluginState {
        name: RefCell::new(String::new()),
    })) as *mut c_void
}

extern "C" fn null_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from null_create's Box::into_raw and is
        // released exactly once by the VM.
        drop(unsafe { Box::from_raw(instance as *mut NullPluginState) });
    }
}

/// Class-name constructor: record the plugin name and enable `CallMissing`.
extern "C" fn null_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    if !instance.is_null()
        && let Some(name) = argv.first()
    {
        // SAFETY: instance is the live payload of this constructor call.
        let state = unsafe { &*(instance as *const NullPluginState) };
        *state.name.borrow_mut() = value_as_string(name);
    }
    if !objthis.is_null()
        && let Some(resolved) = binding::resolve(engine as *mut Engine)
    {
        let host = resolved.engine();
        if let Ok(object) = host.retain_object_detached(objthis) {
            // Best-effort: a failure here would only mean the null
            // object is not catch-all; the call itself never fails.
            let _ = host.set_call_missing(object.raw_id());
        }
    }
    set_void(out);
    0
}

/// The catch-all: log every absent member access, hand back a no-op callable
/// for gets/calls, and report the access as handled.
extern "C" fn null_missing(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let get_or_set = argv.first().map(|value| value.integer).unwrap_or(0);
    let member = argv.get(1).map(value_as_string).unwrap_or_default();
    let name = if instance.is_null() {
        "<unnamed>".to_string()
    } else {
        // SAFETY: instance is the live payload the VM passed in.
        unsafe { &*(instance as *const NullPluginState) }
            .name
            .borrow()
            .clone()
    };
    log::warn!("null plugin {name:?}: ignoring member {member:?} (getorset={get_or_set})");
    log::debug!(
        "null plugin {name:?}: missing(getorset={get_or_set}, name={member:?}, value={:?})",
        argv.get(2).map(value_as_string).unwrap_or_default()
    );
    // A get is also how the VM resolves a *call* to an absent member, so hand
    // back a no-op callable; a set is ignored.
    if get_or_set == 0 {
        ignore_absent_member(engine, argv.get(2));
    }
    // `true` = handled, so the VM does not raise.
    set_bool(out, true);
    0
}

/// Install a no-op callable as the default member of the property object the
/// VM passed to `missing`, so an absent member can be *called* (doc 08
/// §4.2). Failure is non-fatal: the access has already been logged.
///
/// The no-op closure is created once at install time
/// (`CompatServices::ensure_noop_closure`); here we only *duplicate* the
/// cached retention, so the hot path never re-enters the VM and never
/// clobbers the engine's "last object" slot.
fn ignore_absent_member(engine: *mut c_void, prop: Option<&Value>) {
    let Some(prop) = prop else { return };
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return;
    };
    let host = resolved.engine();
    let ctx = resolved.ctx();
    // SAFETY: `resolved` comes from the installed binding, and
    // `ensure_noop_closure` ran during install.
    let Some(cached) = (unsafe { ctx.services() }).noop_closure_id() else {
        return;
    };
    let Ok(noop) = host.duplicate_retained(cached) else {
        return;
    };
    let Ok(prop) = host.retain_object_arg(prop) else {
        return;
    };
    // `set_default_member` consumes the duplicate; the `DetachedValue` drop
    // is then a documented no-op.
    let _ = host.set_default_member(
        prop.raw_id(),
        &TjsValue::Retained(noop.raw_id().as_ptr() as u64),
    );
}

/// Register the `NullPlugin` native instance class (idempotent per VM: the
/// host registers it once at startup).
pub(crate) fn register_hook(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: NULL_PLUGIN_CLASS,
        create: null_create,
        destroy: null_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: NULL_PLUGIN_CLASS,
                f: null_ctor,
            },
            NativeInstanceMethodDef {
                name: "missing",
                f: null_missing,
            },
        ],
        properties: vec![],
    })
}

/// Create a null plugin object for `plugin_name` and install it as a script
/// global under the plugin stem — unless that global already exists.
///
/// Returns `Ok(true)` when the null object became `global[stem]` (the caller
/// records this so [`remove`] knows the link owns the binding) and
/// `Ok(false)` when an existing binding was kept: a native class or script
/// global that happens to share the stem is **never clobbered** —
/// a real binding must not be silently replaced by the null path. The link
/// is recorded either way, and [`remove`] only clears globals whose link
/// installed them.
///
/// The probe uses `typeof` because it is the one read that cannot raise for
/// an absent member: the VM maps a failed property lookup to the string
/// `"undefined"` (`VM_TYPEOFI` → `TypeOfMemberIndirect`), whereas a plain
/// read of an absent `global[stem]` raises member-not-found. A member that
/// exists but holds `void` counts as absent too (a script may clear it by
/// hand; [`remove`]'s `delete` leaves no member at all). `plugin_name` keeps
/// the script's original casing so the global is `windowEx`, not `windowex`
/// (doc 08 §4.2).
pub(crate) fn install(ctx: &mut CompatCtx, plugin_name: &str) -> Result<bool, CompatError> {
    let stem = crate::plugin_stem(plugin_name);
    let stem_lit = tjs_string_literal(&stem);
    let name_lit = tjs_string_literal(plugin_name);
    // SAFETY: `ctx` is the bound, live engine.
    let engine = unsafe { ctx.engine() };
    let kind = engine
        .eval(&format!("typeof global[{stem_lit}]"), "null_plugin")
        .map_err(|err| CompatError::Register(err.to_string()))?;
    let TjsValue::String(kind) = kind else {
        return Err(CompatError::Register(format!(
            "null plugin: typeof global[{stem:?}] did not yield a type name"
        )));
    };
    if kind != "undefined" && kind != "void" {
        log::warn!(
            "null plugin {plugin_name:?}: global {stem:?} already holds a value of type \
             {kind}; keeping it instead of installing the null object"
        );
        return Ok(false);
    }
    // `eval_retained` compiles an *expression*, so the assignment yields the
    // object itself (an `exec_script` statement would yield void).
    let script = format!("global[{stem_lit}] = new {NULL_PLUGIN_CLASS}({name_lit})");
    // SAFETY: `ctx` is the bound, live engine; the expression creates the
    // object and assigns the global, which keeps it alive.
    match engine.eval_retained(&script, "null_plugin") {
        Ok(RetainedValue::Object(object)) => {
            // Only the script global holds the object from here on; release
            // the retention this expression produced.
            drop(object);
            log::debug!("null plugin {plugin_name:?}: installed global {stem:?}");
            Ok(true)
        }
        Ok(RetainedValue::Value(_)) => Err(CompatError::Register(
            "null plugin: the constructor did not return an object".into(),
        )),
        Err(err) => Err(CompatError::Register(err.to_string())),
    }
}

/// Remove a null plugin's script global (`Plugins.unlink`), but only when it
/// still is the null object this module installed.
///
/// The caller passes only the links that actually installed their global (a
/// link that kept a pre-existing binding never calls here), and the guard
/// below re-verifies the binding before deleting: a global a script wrote
/// over — or an object without a class chain, e.g. a function the script put
/// in our place — is left untouched. The object's native class cannot be
/// unregistered either way; a later `link` re-installs the null object when
/// the stem is free again.
pub(crate) fn remove(ctx: &mut CompatCtx, plugin_name: &str) -> Result<(), CompatError> {
    let stem = crate::plugin_stem(plugin_name);
    let stem_lit = tjs_string_literal(&stem);
    // SAFETY: `ctx` is the bound, live engine.
    let engine = unsafe { ctx.engine() };
    // `typeof` never raises for an absent member (see `install`).
    let kind = engine
        .eval(&format!("typeof global[{stem_lit}]"), "null_plugin")
        .map_err(|err| CompatError::Register(err.to_string()))?;
    let TjsValue::String(kind) = kind else {
        return Err(CompatError::Register(format!(
            "null plugin: typeof global[{stem:?}] did not yield a type name"
        )));
    };
    match kind.as_str() {
        // Nothing there: already removed, or cleared by a script.
        "undefined" | "void" => return Ok(()),
        // Only an object can be the null instance we installed.
        "Object" => {}
        scalar => {
            log::debug!(
                "null plugin {plugin_name:?}: global {stem:?} now holds a {scalar}; leaving it"
            );
            return Ok(());
        }
    }
    // `instanceof` takes a class-name string (reference `IsInstanceOf`) and
    // can raise `TJS_E_NOTIMPL` for objects without a class chain — a
    // function the script put where our instance was, where the reference's
    // `IsInstanceOf` simply reports false. The guard therefore runs in a
    // try/catch (the same pattern as the saveStruct serializer): a raised
    // check leaves the binding alone rather than deleting someone else's
    // global.
    let class_lit = tjs_string_literal(NULL_PLUGIN_CLASS);
    let script = format!(
        "try {{ if (global[{stem_lit}] instanceof {class_lit}) delete global[{stem_lit}]; }} catch (e) {{}}"
    );
    // SAFETY: `ctx` is the bound, live engine; `exec_script` compiles
    // statements (unlike `eval`, which compiles expressions only).
    engine
        .exec_script(&script, "null_plugin")
        .map(|_| ())
        .map_err(|err| CompatError::Register(err.to_string()))
}

/// Quote a string as a TJS single-quoted literal.
///
/// Control characters are emitted as four-digit `\xHHHH` escapes: the lexer
/// consumes up to `sizeof(tjs_char)*2` hex digits, so a fixed width keeps a
/// following hex digit from being absorbed into the escape.
fn tjs_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\x{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('\'');
    out
}

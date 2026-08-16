//! No-op native classes for game plugin base classes.
//!
//! The game's scripts declare `class X extends <PluginBase>` for base
//! classes provided by the bundled plugins (`menu.dll`, `windowEx.dll`,
//! `fstat.dll`, ...) that krkr-rs stubs. With no plugin loaded, the
//! `extends` clause fails to resolve. Every one of these bases is used only
//! as a superclass marker or a thin interface (script-defined subclasses
//! provide the real members), so a shared no-op instance class with a
//! class-name constructor hook is faithful: instances chain through it to
//! the script classes and the natives behind them.
//!
//! Verified against the game's scripts (system/*.tjs + patch) — the only
//! `extends` bases that are neither script classes nor registered natives:
//!
//!   InputNotifyBase        SceneBase, EyeCatchBase, StaffRoll extend it
//!   WaveSoundBuffer        SoundBuffer extends it (sound.tjs)
//!   VideoOverlay           MovieLayer extends it (movie.tjs)
//!   WIN32GenericDialogEX   k2compat dialog base
//!   TextContentModelessDialog  k2compat dialog base
//!   SubMenu / SliderV      dialog widgets

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use crate::set_void_out;

#[derive(Default)]
struct NoopInst;

extern "C" fn noop_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(NoopInst)) as *mut c_void
}

extern "C" fn noop_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from noop_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut NoopInst) });
    }
}

/// Class-name constructor hook: accepts any args, returns void.
extern "C" fn noop_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

fn register_one(engine: &Tjs2Engine, name: &'static str) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name,
        create: noop_create,
        destroy: noop_destroy,
        methods: vec![NativeInstanceMethodDef {
            name, // class-name ctor hook
            f: noop_ctor,
        }],
        properties: vec![],
    })
}

/// Register every stub plugin base class (idempotent per name).
pub fn register_plugin_stubs(engine: &Tjs2Engine) -> Result<(), String> {
    for name in [
        "InputNotifyBase",
        "WaveSoundBuffer",
        "VideoOverlay",
        "WIN32GenericDialogEX",
        "TextContentModelessDialog",
        "SubMenu",
        "SliderV",
    ] {
        register_one(engine, name)?;
    }
    Ok(())
}

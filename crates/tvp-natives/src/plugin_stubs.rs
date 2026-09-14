//! No-op native classes for game plugin base classes.
//!
//! The game's scripts declare `class X extends <PluginBase>` for base
//! classes provided by the bundled plugins (`menu.dll`, `windowEx.dll`,
//! `fstat.dll`, ...) that krkr-rs replaces with built-in natives. With no
//! plugin loaded, the `extends` clause fails to resolve, so each base is
//! registered as an instance class whose constructor accepts any arguments.
//!
//! # Verification (both shipped games' scripts)
//!
//! The bases below are only used as superclass markers or are reached only
//! through plugin paths the emulator replaces; no script calls a *native*
//! member of these classes on a path the engine exercises:
//!
//! | base | where used | native members called? |
//! |---|---|---|
//! | `InputNotifyBase` | `system/eyecatch.tjs` (`class EyeCatchBase extends
//!   ActivateLayer, InputNotifyBase`) | no — marker mixin; input is routed by
//!   `Window.addInputNotify`/`removeInputNotify` |
//! | `WIN32GenericDialogEX` | `k2compat/k2compat.tjs` checks only
//!   `typeof global.WIN32GenericDialogEX` to decide whether to load the
//!   real `win32dialog.tjs` | no — its dialog members (`addLText`,
//!   `addLineInput`, ...) are reached only from `System.inputString`, which
//!   neither game calls (the k2compat override is never forced) |
//! | `TextContentModelessDialog` | `k2compat/k2compat_padcommon.tjs` (loaded
//!   only from the optional `Pad`/`Debug.console` delay-loaders) | no |
//! | `SubMenu` | `system/messageframe.tjs` (superclass of `SystemMenu`,
//!   `QuickSaveMenu`, `JumpMenu`, `ConfigSubMenu`) | **inherited Layer
//!   members only** (`setSize`, `loadImages`, `copyRect`, ...); see below |
//! | `SliderV` | not referenced by either game's scripts | no |
//!
//! # `SubMenu`
//!
//! `menu.dll`'s `SubMenu` is a `Layer` subclass in the reference, so its
//! script subclasses (`SystemMenu`, ...) call inherited `Layer` members plus
//! the plugin's `addItem`/`join`. A bare instance-class stub cannot express
//! native inheritance through the current `tjs2-sys` builder (no base-class
//! field), and `addItem`/`join` belong to the menu registry in
//! `menu_item.rs`. Both are outside this file's scope and are reported rather
//! than papered over. In the shipped games the whole path is latent: it runs
//! only if `MessageFrame` is instantiated, and neither game's scripts do that
//! (only `LoadScript("MessageFrame.tjs")` defines the class).
//!
//! `VideoOverlay` (the base of `MovieLayer` in movie.tjs) is deliberately
//! NOT registered here: the game calls real members on it (`open`, `play`,
//! `originalWidth`, ...), so it has a dedicated non-throwing stub in
//! [`crate::video_overlay`].

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
        invalidate: None,
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
        "WIN32GenericDialogEX",
        "TextContentModelessDialog",
        "SubMenu",
        "SliderV",
    ] {
        register_one(engine, name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;
    use tjs2_sys::Tjs2Engine;

    #[test]
    fn stub_plugin_bases_resolve_and_can_be_extended() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        register_plugin_stubs(&engine).expect("register plugin stubs");
        // Every stub is a usable superclass marker and instance base.
        engine
            .exec_script(
                "class SceneBaseX extends InputNotifyBase {} \
                 class DlgX extends WIN32GenericDialogEX {} \
                 class TextX extends TextContentModelessDialog {} \
                 class SubX extends SubMenu {} \
                 class SliderX extends SliderV {} \
                 new SceneBaseX(); new DlgX(); new TextX(); new SubX(); new SliderX();",
                "plugin_stubs",
            )
            .expect("stub plugin bases must resolve and construct");
    }
}

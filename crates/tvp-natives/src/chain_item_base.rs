//! `ChainItemBase` native class stub.
//!
//! The game's `system/ADVScreen.tjs` declares
//! `class ADVScreen extends Layer, SelectItemNotifyBase, SceneBase,
//! ChainItemBase` — `ChainItemBase` is provided by one of the game's bundled
//! plugins (like `MenuItem` from menu.dll). With plugins stubbed, the class
//! must exist for the `extends` clause to resolve. It is used ONLY as a
//! superclass marker (no member calls anywhere in the game's scripts), so a
//! no-op native class is faithful enough: instances chain through it to the
//! script superclasses and the native `Layer` behind them.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use crate::set_void_out;

/// No state needed — the class exists so `extends ChainItemBase` resolves.
#[derive(Default)]
struct ChainItemBaseInst;

extern "C" fn chain_item_base_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(ChainItemBaseInst)) as *mut c_void
}

extern "C" fn chain_item_base_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from chain_item_base_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut ChainItemBaseInst) });
    }
}

/// `ChainItemBase(...)` / `super.ChainItemBase(...)` — no-op constructor.
extern "C" fn chain_item_base_ctor(
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

/// Register the stub `ChainItemBase` native class on the engine's global.
pub fn register_chain_item_base(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "ChainItemBase",
        create: chain_item_base_create,
        destroy: chain_item_base_destroy,
        methods: vec![NativeInstanceMethodDef {
            name: "ChainItemBase", // class-name ctor hook
            f: chain_item_base_ctor,
        }],
        properties: vec![],
    })
}

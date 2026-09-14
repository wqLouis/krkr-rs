//! `ChainItemBase` native class — registration fallback, memberless.
//!
//! The game's `system/ADVScreen.tjs` declares
//! `class ADVScreen extends Layer, SelectItemNotifyBase, SceneBase,
//! ChainItemBase`. That name is **not** provided by any native plugin: the
//! game defines the real class itself in `system/selectitem.tjs` (line 2049
//! in the shipped `data.xp3`):
//!
//! ```text
//! class ChainItemBase{
//!     var _chainIndex = 0;
//!     var _chainItem = [];
//!     function addChainItem(obj){ ... }
//!     function removeChainItem(obj){ ... }
//!     function removeChainItemAll(){ ... }
//!     function onKeyDown(key, shift){ ... }
//!     function mouseTracking(obj){ ... }
//!     ...
//! }
//! ChainItemBase.MOUSETRACKINGMODE_SEQUENTIAL = 0;
//! ChainItemBase.MOUSETRACKINGMODE_FREE = 1;
//! ```
//!
//! `system/initialize.tjs` loads `SelectItem.tjs` **before** `ADVObject.tjs`
//! / `ADVScreen.tjs`, so by the time the `extends` clause is evaluated the
//! script class exists and the calls the game makes
//! (`global.ChainItemBase.onKeyDown(...)`,
//! `global.ChainItemBase.MOUSETRACKINGMODE_SEQUENTIAL`,
//! `global.ChainItemBase.mouseTracking(...)`,
//! `global.ChainItemBase.finalize()`) all resolve to the **script** class.
//!
//! This native registration is therefore kept only as a *forward-compatible
//! fallback*: it makes `class X extends ChainItemBase` parse and instantiate
//! even when the game's `SelectItem.tjs` is absent or loaded later (other
//! KAG-era titles), without changing the semantics of the script class that
//! shadows it. It deliberately has **no members**: implementing any here
//! would be dead code, because the script class replaces this class object on
//! the global before any member is ever called, and the audit of every `.tjs`
//! / `.ks` in `data.xp3` + `patch.xp3` found no member lookup that could
//! reach the native payload.

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
/// The script class that shadows this registration also defines its own
/// parameterless constructor, so the native body only needs to complete the
/// object graph (the reference native-instance constructor contract).
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

/// Register the fallback `ChainItemBase` native class on the engine's global.
pub fn register_chain_item_base(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "ChainItemBase",
        create: chain_item_base_create,
        destroy: chain_item_base_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "ChainItemBase", // class-name ctor hook
            f: chain_item_base_ctor,
        }],
        properties: vec![],
    })
}

#[cfg(test)]
mod tests {
    use tjs2_sys::TjsValue;

    use crate::test_lock::vm_lock;

    use super::*;

    /// A script subclass can `extends ChainItemBase` and construct through the
    /// native fallback when no script class defines the name.
    #[test]
    fn script_subclass_extends_the_fallback() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        register_chain_item_base(&e).expect("register ChainItemBase");
        e.exec_script(
            "class Probe extends ChainItemBase { function Probe(){} function tag(){ return 7; } } \
             var p = new Probe();",
            "chain_item_base_test",
        )
        .expect("script subclass must parse and construct");
        assert_eq!(e.eval("p.tag()", "test").unwrap(), TjsValue::Integer(7));
    }

    /// The game supplies the real members in `system/selectitem.tjs`; a script
    /// `class ChainItemBase` shadows the native fallback and its members win.
    #[test]
    fn script_definition_shadows_the_native_fallback() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        register_chain_item_base(&e).expect("register ChainItemBase");
        e.exec_script(
            "class ChainItemBase { function ChainItemBase(){} function addChainItem(o){ return o + 1; } } \
             var n = new ChainItemBase();",
            "chain_item_base_test",
        )
        .expect("script class definition must replace the native class");
        assert_eq!(
            e.eval("n.addChainItem(41)", "test").unwrap(),
            TjsValue::Integer(42)
        );
    }
}

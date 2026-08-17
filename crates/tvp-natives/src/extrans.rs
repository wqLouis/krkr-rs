//! `extrans.dll` plugin surface — `Trans` class stub + transition registry.
//!
//! The real `extrans` plugin (krkrz/SamplePlugin, also in the old krkr2
//! tree) does **not** register a `Trans` native class. `V2Link` only
//! registers transition **handler providers** into the engine's global
//! registry (`TVPAddTransHandlerProvider`), which `Layer.beginTransition`
//! consults by name:
//!
//! | provider | name | options |
//! |---|---|---|
//! | wave | `wave` | `time`, `maxh`, `maxomega`, `bgcolor1`, `bgcolor2`, `wavetype` |
//! | mosaic | `mosaic` | `time`, `maxsize` |
//! | turn | `turn` | `time`, `bgcolor` |
//! | rotatezoom | `rotatezoom` | `time`, `factor`, `accel`, `twist`, `twistaccel`, `centerx`, `centery` |
//! | rotatevanish | `rotatevanish` | `time`, `accel`, `twist`, `twistaccel`, `centerx`, `centery` |
//! | rotateswap | `rotateswap` | `time`, `accel`, `twist`, `twistaccel`, `centerx`, `centery` |
//! | ripple | `ripple` | `time`, `centerx`, `centery` |
//!
//! Every provider reports `ttExchange` (two-layer cross-exchange) with
//! `tutDivisible` update semantics (verified against both the current
//! SamplePlugin and the krkr2 plugin tree). The engine's own default
//! providers (`TVPRegisterDefaultTransHandlerProvider`, TransIntf.cpp) are
//! `crossfade` (ttExchange, tutDivisibleFade), `universal` (ttExchange,
//! tutDivisibleFade) and `scroll` (ttExchange, tutDivisible).
//!
//! # `Trans` class
//!
//! The `Trans` class this stub provides is a **compat no-op**: the task
//! description asked for it, and old KAG-era scripts sometimes do
//! `new Trans(layer, options)` / `.start()`. The modern engine has no such
//! native class (the transition surface is `Layer.beginTransition` /
//! `Layer.stopTransition`), so the class exists so those scripts parse and
//! run, but it performs no pixel work. The audit (data.xp3 + patch.xp3,
//! every `.tjs`/`.ks`, k2compat, patch.tjs) found **no** `Trans`
//! instantiation and no `Layer.trans` usage — this game's transitions all
//! go through `Layer.beginTransition` with the names above.
//!
//! # Registry
//!
//! [`transition_kind`] exposes the name → `(trans_type, update_type)` table
//! so `Layer.beginTransition` (tvp-visual) can validate/classify names
//! without reimplementing the plugin's registration. The values mirror
//! `tTVPTransType` (`ttSimple`=0, `ttExchange`=1) and `tTVPTransUpdateType`
//! (`tutDivisibleFade`=0, `tutDivisible`=1, `tutGiveUpdate`=2) from
//! `reference/cpp/core/visual/transhandler.h`.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use crate::set_void_out;

/// `tTVPTransType` values (transhandler.h): `ttSimple` uses only the self
/// layer; `ttExchange` blends two layers (source 1 + source 2).
pub const TRANS_TYPE_SIMPLE: i64 = 0;
pub const TRANS_TYPE_EXCHANGE: i64 = 1;

/// `tTVPTransUpdateType` values (transhandler.h): how the handler consumes
/// its source rectangles during `Process`.
pub const TRANS_UPDATE_DIVISIBLE_FADE: i64 = 0;
pub const TRANS_UPDATE_DIVISIBLE: i64 = 1;
pub const TRANS_UPDATE_GIVE_UPDATE: i64 = 2;

/// Transition metadata: the classification a provider reports from
/// `StartTransition` (transhandler.h `tTVPTransType` / `tTVPTransUpdateType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionKind {
    pub trans_type: i64,
    pub update_type: i64,
}

/// The engine's built-in transition names, registered by
/// `TVPRegisterDefaultTransHandlerProvider` (TransIntf.cpp).
pub fn builtin_transition_kind(name: &str) -> Option<TransitionKind> {
    match name {
        // crossfade: tutDivisibleFade (region-divisible, source-restricted)
        "crossfade" => Some(TransitionKind {
            trans_type: TRANS_TYPE_EXCHANGE,
            update_type: TRANS_UPDATE_DIVISIBLE_FADE,
        }),
        // universal: same family as crossfade (tutDivisibleFade)
        "universal" => Some(TransitionKind {
            trans_type: TRANS_TYPE_EXCHANGE,
            update_type: TRANS_UPDATE_DIVISIBLE_FADE,
        }),
        // scroll: tutDivisible (any source area; the handler scrolls both
        // layers, so the caller must not pre-restrict the source rect)
        "scroll" => Some(TransitionKind {
            trans_type: TRANS_TYPE_EXCHANGE,
            update_type: TRANS_UPDATE_DIVISIBLE,
        }),
        _ => None,
    }
}

/// The `extrans.dll` transition names — every provider registers
/// `ttExchange` / `tutDivisible` (verified against the plugin source).
pub fn extrans_transition_kind(name: &str) -> Option<TransitionKind> {
    const EXTRANS: [&str; 7] = [
        "wave",
        "mosaic",
        "turn",
        "rotatezoom",
        "rotatevanish",
        "rotateswap",
        "ripple",
    ];
    EXTRANS.contains(&name).then_some(TransitionKind {
        trans_type: TRANS_TYPE_EXCHANGE,
        update_type: TRANS_UPDATE_DIVISIBLE,
    })
}

/// Look up any transition name the game can pass to `Layer.beginTransition`
/// (builtin core providers first, then the extrans plugin's). Returns `None`
/// for unknown names — the reference throws `TVPCannotFindTransHander` in
/// that case, so callers decide whether to error.
pub fn transition_kind(name: &str) -> Option<TransitionKind> {
    builtin_transition_kind(name).or_else(|| extrans_transition_kind(name))
}

/// Payload for the compat `Trans` no-op class.
#[derive(Default)]
struct TransInst;

extern "C" fn trans_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<TransInst>::default()) as *mut c_void
}

extern "C" fn trans_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from trans_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut TransInst) });
    }
}

/// `Trans(layer, options)` — compat constructor. The real KAG-era class
/// wrapped a full-screen transition; this stub accepts anything and stores
/// nothing (the game never instantiates it).
extern "C" fn trans_ctor(
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

/// `start()` — no-op in the stub (see the module doc).
extern "C" fn trans_start(
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

/// Register the compat `Trans` native class (no-op instance class).
pub fn register_trans(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Trans",
        create: trans_create,
        destroy: trans_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Trans", // class-name ctor hook
                f: trans_ctor,
            },
            NativeInstanceMethodDef {
                name: "start",
                f: trans_start,
            },
        ],
        properties: vec![],
    })
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;

    #[test]
    fn extrans_names_are_exchange_divisible() {
        let _vm_lock = vm_lock();
        for name in [
            "wave",
            "mosaic",
            "turn",
            "rotatezoom",
            "rotatevanish",
            "rotateswap",
            "ripple",
        ] {
            let kind = extrans_transition_kind(name).expect("extrans name");
            assert_eq!(kind.trans_type, TRANS_TYPE_EXCHANGE);
            assert_eq!(kind.update_type, TRANS_UPDATE_DIVISIBLE);
        }
    }

    #[test]
    fn builtin_names_are_registered() {
        let _vm_lock = vm_lock();
        assert_eq!(
            builtin_transition_kind("crossfade"),
            Some(TransitionKind {
                trans_type: TRANS_TYPE_EXCHANGE,
                update_type: TRANS_UPDATE_DIVISIBLE_FADE,
            })
        );
        assert_eq!(
            builtin_transition_kind("universal"),
            Some(TransitionKind {
                trans_type: TRANS_TYPE_EXCHANGE,
                update_type: TRANS_UPDATE_DIVISIBLE_FADE,
            })
        );
        assert_eq!(
            builtin_transition_kind("scroll"),
            Some(TransitionKind {
                trans_type: TRANS_TYPE_EXCHANGE,
                update_type: TRANS_UPDATE_DIVISIBLE,
            })
        );
    }

    #[test]
    fn unknown_transition_names_are_not_registered() {
        let _vm_lock = vm_lock();
        assert!(transition_kind("no-such-transition").is_none());
        assert!(transition_kind("").is_none());
    }

    #[test]
    fn trans_class_registers_and_constructs() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        register_trans(&e).expect("register Trans");
        // Compat surface: construct + start, both no-ops.
        e.exec_script("var t = new Trans(); t.start();", "extrans_test")
            .unwrap();
        assert!(e.eval("t", "extrans_test").is_ok());
    }
}

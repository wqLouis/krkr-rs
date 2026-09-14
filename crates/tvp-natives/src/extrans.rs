//! `extrans.dll` plugin surface — transition registry + a real `Trans`
//! driver wired to the engine's transition machinery.
//!
//! # The plugin registry
//!
//! The real `extrans` plugin (`krkrz/SamplePlugin/extrans`) does **not**
//! register a `Trans` native class. Its `V2Link` only registers transition
//! **handler providers** into the engine's global registry
//! (`TVPAddTransHandlerProvider`), which `Layer.beginTransition` consults by
//! name:
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
//! `tutDivisible` update semantics (verified against the current
//! SamplePlugin source and the krkr2 plugin tree). The engine's own default
//! providers (`TVPRegisterDefaultTransHandlerProvider`, TransIntf.cpp) are
//! `crossfade` (ttExchange, tutDivisibleFade), `universal` (ttExchange,
//! tutDivisibleFade) and `scroll` (ttExchange, tutDivisible).
//!
//! # `Trans` class
//!
//! `Trans` is a krkr-rs compatibility class (the shipped game links
//! `extrans.dll` but the audit of every `.tjs`/`.ks` in `data.xp3` +
//! `patch.xp3` found no `new Trans` / `Trans(...)` use; this game drives
//! transitions through `Layer.beginTransition` +
//! `setTransitionCompleteCall`). It is **not** a no-op: `Trans(layer,
//! options)` captures the target layer and options and `start()` drives a
//! real transition through the engine's transition machinery:
//!
//! * [`start`](Trans) calls the target layer's native/native-script
//!   `beginTransition(name, withchildren, transwith, options)` (the same
//!   entry point `Layer.beginTransition` exposes, `LayerIntf.cpp:9815`).
//! * The engine's per-frame `transition_poll` then delivers
//!   `onTransitionCompleted(dest, src)` to the layer, exactly as it does for
//!   a direct `beginTransition` call.
//! * `Trans` additionally tracks the transition duration (`options.time`,
//!   ms) on the same per-frame clock and invokes its own completion callback
//!   (`options.callback` / `options.onComplete`) once, when the duration
//!   elapses (or on the next poll for `time == 0`). [`stop`](Trans) cancels
//!   the transition (`Layer.stopTransition`); `complete()` forces it.
//!
//! `isCompleted` is a read-only property (used by scripts/tests to observe
//! the lifecycle).
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
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, TjsValue, Value,
};

use crate::{args, context_engine, lock_ok, report_error, set_int_out, set_void_out};

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

// ---------------------------------------------------------------------------
// `Trans` transition driver
// ---------------------------------------------------------------------------

/// One live `Trans` object. Retains every script value it must keep alive
/// across calls (the target layer, the options dict, the optional
/// `transwith` layer and completion callback).
#[derive(Default)]
struct TransInst {
    /// Target `Layer` object (`Trans(layer, ...)`) whose `beginTransition`
    /// the engine will drive.
    layer: Option<DetachedValue>,
    /// The options dictionary, kept alive so `beginTransition` can receive
    /// it as an argument.
    options: Option<DetachedValue>,
    /// Optional second layer (`options.transwith`) for the exchange.
    trans_with: Option<DetachedValue>,
    /// Optional completion callback (`options.callback` / `onComplete`).
    callback: Option<DetachedValue>,
    /// Transition name (default `"crossfade"`).
    name: String,
    /// `options.withchildren` (default true).
    with_children: bool,
    /// `options.time` in milliseconds (0 = one poll).
    time_ms: i64,
    /// Engine tick at `start()`; `None` when not started (or completed).
    started_ms: Option<i64>,
    /// Set once the completion callback has fired.
    completed: bool,
}

/// Addresses of started `Trans` objects, advanced once per frame by
/// [`trans_poll`]. `destroy` unregisters before the payload is freed, so a
/// pointer in this set is always live.
static ACTIVE_TRANS: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn unregister(inst: *mut TransInst) {
    let addr = inst as usize;
    lock_ok(&ACTIVE_TRANS).retain(|&a| a != addr);
}

/// Whether an ABI argument carries a non-null object handle.
fn is_object(v: &Value) -> bool {
    v.ty == tjs2_sys::VAL_OBJECT && v.retained != 0
}

/// TJS `operator bool` for a `get_member` result.
fn tjs_bool(v: &TjsValue) -> bool {
    match v {
        TjsValue::Void => false,
        TjsValue::Integer(i) => *i != 0,
        TjsValue::Real(r) => *r != 0.0,
        TjsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn option_value(engine: &Tjs2Engine, obj: tjs2_sys::Tjs2ValueId, name: &str) -> Option<TjsValue> {
    engine
        .get_member(obj, name)
        .ok()
        .filter(|v| !matches!(v, TjsValue::Void))
}

fn option_string(
    engine: &Tjs2Engine,
    obj: tjs2_sys::Tjs2ValueId,
    names: &[&str],
) -> Option<String> {
    names
        .iter()
        .find_map(|name| match option_value(engine, obj, name) {
            Some(TjsValue::String(s)) => Some(s),
            _ => None,
        })
}

fn option_int(engine: &Tjs2Engine, obj: tjs2_sys::Tjs2ValueId, name: &str) -> Option<i64> {
    match option_value(engine, obj, name) {
        Some(TjsValue::Integer(i)) => Some(i),
        Some(TjsValue::Real(r)) => Some(r as i64),
        _ => None,
    }
}

/// Retain an object-valued option. [`Tjs2Engine::get_member`] cannot carry an
/// object handle, but the C++ side records the most recent object result in
/// `last_object`, so [`Tjs2Engine::retain_value_detached`] resolves it right
/// after the getter. Returns the first present name.
fn option_object(
    engine: &Tjs2Engine,
    obj: tjs2_sys::Tjs2ValueId,
    names: &[&str],
) -> Option<DetachedValue> {
    for name in names {
        if let Ok(TjsValue::Object) = engine.get_member(obj, name)
            && let Ok(dv) = engine.retain_value_detached(&TjsValue::Object)
        {
            return Some(dv);
        }
    }
    None
}

extern "C" fn trans_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<TransInst>::default()) as *mut c_void
}

extern "C" fn trans_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let ptr = instance as *mut TransInst;
    unregister(ptr);
    // SAFETY: instance came from trans_create's Box::into_raw.
    drop(unsafe { Box::from_raw(ptr) });
}

/// `Trans(layer [, options])` — capture the target layer and options. The
/// layer and options objects are retained for the object's lifetime; scalar
/// options are read once, object options (`transwith`, callback) are retained
/// through [`option_object`].
extern "C" fn trans_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let engine = context_engine();
    // SAFETY: `instance` is a live TransInst payload.
    let inst = unsafe { &mut *(instance as *mut TransInst) };
    inst.name = "crossfade".to_string();
    inst.with_children = true;

    if let Some(value) = values.first().filter(|v| is_object(v)) {
        match engine.retain_object_arg(value) {
            Ok(dv) => inst.layer = Some(dv),
            Err(e) => {
                return report_error(out_error, &format!("Trans: cannot retain layer: {e}"));
            }
        }
    }
    if let Some(value) = values.get(1).filter(|v| is_object(v)) {
        match engine.retain_object_arg(value) {
            Ok(dv) => {
                if let Some(name) = option_string(engine, dv.raw_id(), &["name", "method"]) {
                    inst.name = name;
                }
                if let Some(time) = option_int(engine, dv.raw_id(), "time") {
                    inst.time_ms = time.max(0);
                }
                if let Some(with_children) = option_value(engine, dv.raw_id(), "withchildren") {
                    inst.with_children = tjs_bool(&with_children);
                }
                inst.trans_with = option_object(engine, dv.raw_id(), &["transwith"]);
                inst.callback = option_object(engine, dv.raw_id(), &["callback", "onComplete"]);
                inst.options = Some(dv);
            }
            Err(e) => {
                return report_error(out_error, &format!("Trans: cannot retain options: {e}"));
            }
        }
    }
    set_void_out(out);
    0
}

/// `start()` — begin the transition on the target layer through the engine's
/// `beginTransition` entry point, then track it on the per-frame clock. With
/// no target layer the object still completes (its callback fires once), so
/// `new Trans().start()` stays a well-defined lifecycle.
extern "C" fn trans_start(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live TransInst payload.
    let inst = unsafe { &mut *(instance as *mut TransInst) };
    if inst.started_ms.is_some() {
        set_void_out(out);
        return 0;
    }
    let engine = context_engine();
    // Mark started and register before the engine call: the retained
    // `options`/`transwith` arguments are consumed by `beginTransition`, so a
    // retry must see `None` rather than a stale retained id.
    inst.started_ms = Some(crate::system::tick_count_ms());
    {
        let addr = instance as usize;
        let mut active = lock_ok(&ACTIVE_TRANS);
        if !active.contains(&addr) {
            active.push(addr);
        }
    }
    if let Some(layer) = &inst.layer {
        let trans_with = inst.trans_with.take();
        let options = inst.options.take();
        let mut call_args = vec![
            TjsValue::String(inst.name.clone()),
            TjsValue::Integer(i64::from(inst.with_children)),
        ];
        call_args.push(match &trans_with {
            Some(dv) => TjsValue::Retained(dv.raw_id() as u64),
            None => TjsValue::Void,
        });
        call_args.push(match &options {
            Some(dv) => TjsValue::Retained(dv.raw_id() as u64),
            None => TjsValue::Void,
        });
        if let Err(e) = engine.call_member(layer.raw_id(), "beginTransition", &call_args) {
            return report_error(
                out_error,
                &format!("Trans.start: beginTransition failed: {e}"),
            );
        }
    }
    set_void_out(out);
    0
}

/// `stop()` — cancel the transition and drop it from the poll set. The
/// completion callback is **not** fired (the reference's `StopTransition`).
extern "C" fn trans_stop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live TransInst payload.
    let inst = unsafe { &mut *(instance as *mut TransInst) };
    unregister(instance as *mut TransInst);
    inst.started_ms = None;
    if let Some(layer) = &inst.layer {
        let _ = context_engine().call_member(layer.raw_id(), "stopTransition", &[]);
    }
    set_void_out(out);
    0
}

/// `complete()` — force completion and fire the callback once.
extern "C" fn trans_complete(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let ptr = instance as *mut TransInst;
    unregister(ptr);
    fire_completion(context_engine(), ptr);
    set_void_out(out);
    0
}

/// `isCompleted` — whether the completion callback has fired.
extern "C" fn trans_is_completed_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live TransInst payload.
    let inst = unsafe { &*(instance as *const TransInst) };
    set_int_out(out, i64::from(inst.completed));
    0
}

/// Invoke the retained completion callback exactly once.
fn fire_completion(engine: &Tjs2Engine, inst: *mut TransInst) {
    // SAFETY: the caller has removed `inst` from ACTIVE_TRANS and the payload
    // stays alive for the duration of the call (`destroy` only runs after
    // unregistering).
    let inst = unsafe { &mut *inst };
    if inst.completed {
        return;
    }
    inst.completed = true;
    inst.started_ms = None;
    if let Some(callback) = inst.callback.take()
        && let Err(e) = engine.call_detached(&callback, &[])
    {
        log::warn!("Trans completion callback failed: {e}");
    }
}

/// Advance every started `Trans` once per frame (called from
/// [`crate::continuous_handler_poll`], after the engine's own
/// `transition_poll`). A `Trans` completes when its `options.time` has
/// elapsed; `time == 0` completes on the first poll after `start()`.
pub(crate) fn trans_poll(engine: &Tjs2Engine, now_ms: i64) {
    let completed: Vec<usize> = {
        let mut active = lock_ok(&ACTIVE_TRANS);
        let mut done = Vec::new();
        active.retain(|&addr| {
            if addr == 0 {
                return false;
            }
            // SAFETY: addresses in ACTIVE_TRANS point at live payloads;
            // destroy unregisters before freeing.
            let inst = unsafe { &mut *(addr as *mut TransInst) };
            let complete = match inst.started_ms {
                Some(start) => now_ms.saturating_sub(start) >= inst.time_ms,
                None => false,
            };
            if complete {
                done.push(addr);
                false
            } else {
                true
            }
        });
        done
    };
    for addr in completed {
        fire_completion(engine, addr as *mut TransInst);
    }
}

/// Register the `Trans` native class.
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
            NativeInstanceMethodDef {
                name: "stop",
                f: trans_stop,
            },
            NativeInstanceMethodDef {
                name: "complete",
                f: trans_complete,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "isCompleted",
            get: Some(trans_is_completed_get),
            set: None,
        }],
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
        crate::register_all(&e).expect("register all natives");
        // Compat surface: construct + start; no layer means the object still
        // completes on the first poll.
        e.exec_script("var t = new Trans(); t.start();", "extrans_test")
            .unwrap();
        assert!(e.eval("t", "extrans_test").is_ok());
        assert_eq!(
            e.eval("t.isCompleted", "extrans_test").unwrap(),
            TjsValue::Integer(0)
        );
        drop(e);
    }

    /// `start()` really calls the target layer's `beginTransition` and the
    /// completion callback fires on the poll once the duration elapses.
    #[test]
    fn trans_starts_the_engine_transition_and_fires_the_callback() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        crate::register_all(&e).expect("register all natives");
        e.exec_script(
            "var log = []; \
             function onBegin(name, children, withlayer, options) { \
                 log.push(name); log.push(children); log.push(options.name); \
             } \
             function onStopStop() { log.push('stop'); } \
             var layer = %[ beginTransition: onBegin, stopTransition: onStopStop ]; \
             var done = 0; \
             var t = new Trans(layer, %[name:'mosaic', time:0, callback:function(){ done++; }]); \
             t.start();",
            "extrans_test",
        )
        .expect("start must call beginTransition");
        // The engine transition was started with the configured name, the
        // default `withchildren=true` and the options dictionary.
        assert_eq!(
            e.eval("log[0]", "extrans_test").unwrap(),
            TjsValue::String("mosaic".into())
        );
        assert_eq!(
            e.eval("log[1]", "extrans_test").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("log[2]", "extrans_test").unwrap(),
            TjsValue::String("mosaic".into())
        );
        // Not completed until the poll runs.
        assert_eq!(
            e.eval("done", "extrans_test").unwrap(),
            TjsValue::Integer(0)
        );
        trans_poll(&e, crate::system::tick_count_ms());
        assert_eq!(
            e.eval("done", "extrans_test").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("t.isCompleted", "extrans_test").unwrap(),
            TjsValue::Integer(1)
        );
        // Firing is once-only.
        trans_poll(&e, crate::system::tick_count_ms());
        assert_eq!(
            e.eval("done", "extrans_test").unwrap(),
            TjsValue::Integer(1)
        );
        drop(e);
    }

    /// `stop()` cancels without firing the completion callback, and
    /// `complete()` forces it.
    #[test]
    fn trans_stop_and_complete() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        crate::register_all(&e).expect("register all natives");
        e.exec_script(
            "var layer = %[ beginTransition: function(){}, stopTransition: function(){} ]; \
             var done = 0; \
             var a = new Trans(layer, %[time:100000, callback:function(){ done += 1; }]); \
             a.start(); \
             var b = new Trans(layer, %[time:100000, callback:function(){ done += 10; }]); \
             b.start(); \
             a.stop(); \
             b.complete();",
            "extrans_test",
        )
        .unwrap();
        // `a` was cancelled (no callback), `b` fired (10).
        assert_eq!(
            e.eval("done", "extrans_test").unwrap(),
            TjsValue::Integer(10)
        );
        assert_eq!(
            e.eval("a.isCompleted", "extrans_test").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("b.isCompleted", "extrans_test").unwrap(),
            TjsValue::Integer(1)
        );
        // A long-running `a` must not fire later.
        trans_poll(&e, i64::MAX);
        assert_eq!(
            e.eval("done", "extrans_test").unwrap(),
            TjsValue::Integer(10)
        );
        drop(e);
    }
}

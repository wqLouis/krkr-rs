//! Integration tests pinning identifier resolution inside functions that
//! live in a dictionary literal (`%[ f: function() { ... } ]`).
//!
//! An unverified audit report claimed this pure-script repro misbehaves:
//!
//! ```tjs
//! var g = 0;
//! var d = %[ f: function() { g = g + 1; } ];
//! d.f(); d.f();
//! // report: "global g ends at 1, not 2"
//! ```
//!
//! The report assumes a bare identifier in a function body always names the
//! script's global. The vendored reference (`crates/tjs/cpp/tjs2/**`, read
//! only — the authority on what TJS means) shows otherwise:
//!
//! * A name that is not a local of the compiling context is rewritten at
//!   compile time into `this_proxy.name` (`tjsInterCodeGen.cpp:2325-2347`);
//!   the top level of a script is *not* special-cased (`AsGlobalContextMode`
//!   is only raised around `super` codegen, `tjsInterCodeGen.cpp:2479`).
//! * At run time the proxy is `(Dispatch1 = objthis, Dispatch2 = global)`
//!   (`tjsInterCodeExec.cpp:662-667`), and `d.f()` passes `objthis = d`
//!   because an inline dictionary is built by `VM_NEW` into
//!   `tTJSVariant(dsp, dsp)` — the object is its own `ObjThis`
//!   (`tjsInterCodeExec.cpp:2138`) — and `CallFunctionDirect` selects
//!   `clo.ObjThis` over the caller's `this` (`tjsInterCodeExec.cpp:2186-2192`).
//! * The proxy falls back from `Dispatch1` to `Dispatch2` **only** when the
//!   first target reports `TJS_E_MEMBERNOTFOUND`
//!   (`tjsInterCodeExec.cpp:262-268` for get, `282-287` for set,
//!   `430-437` for operations). But
//!   `tTJSDictionaryObject::PropGet` swallows a missing read as
//!   `TJS_S_OK` + void (`tjsDictionary.cpp:739-743`), so a bare *read*
//!   never falls through to the global when the receiver is a dictionary —
//!   it yields void. A plain flag-0 `PropSet`, in contrast, reports
//!   `TJS_E_MEMBERNOTFOUND` for a missing member
//!   (`tjsObject.cpp:1382-1383`), so a bare *write* does fall through to
//!   the global.
//! * `void + 1 == 1` (`tjsVariant.cpp:1191-1199`, `operator+=` maps
//!   void-plus-integer onto the integer), so `g = g + 1` inside `d.f()`
//!   writes the constant `1` to the global on *every* call: the report's
//!   observed `g == 1` is exactly what the reference produces, and the
//!   expected `2` is the wrong model.
//!
//! The variants below discriminate this model from "bare names are always
//! the script global". Each assertion states which rule it demonstrates.

use std::sync::{Mutex, MutexGuard};

use tjs::{Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn engine() -> Tjs2Engine {
    Tjs2Engine::new().unwrap()
}

/// Evaluates `expr` and asserts the integer result.
fn eval_int(engine: &Tjs2Engine, expr: &str) -> i64 {
    match engine.eval(expr, "dict_closure_scope").unwrap() {
        TjsValue::Integer(v) => v,
        other => panic!("expected integer for `{expr}`, got {other:?}"),
    }
}

/// Evaluates `expr` and asserts it is void (a missing dictionary member
/// read through `tTJSDictionaryObject::PropGet` returns `TJS_S_OK` with a
/// cleared result, not an error).
fn eval_is_void(engine: &Tjs2Engine, expr: &str) {
    assert_eq!(
        engine.eval(expr, "dict_closure_scope").unwrap(),
        TjsValue::Void,
        "`{expr}` should resolve to void"
    );
}

/// The exact reported repro, unmodified.
///
/// Observed (and reference-derived): `g == 1`, not `2`.
///
/// Why `1`:
/// * each call's bare read of `g` hits `d` first; the dictionary has no
///   member `g`, and `tTJSDictionaryObject::PropGet` swallows that miss as
///   `TJS_S_OK` + void (`tjsDictionary.cpp:739-743`) — the proxy's
///   `TJS_E_MEMBERNOTFOUND` fallback (`tjsInterCodeExec.cpp:262-268`)
///   therefore never fires, so the read yields *void*, not the global;
/// * `void + 1 == 1` (`tjsVariant.cpp:1191-1199`);
/// * the assignment compiles to `VM_SPD` (flag 0 — this-proxy left-hand
///   sides deliberately skip `TJS_MEMBERENSURE`, `tjsInterCodeGen.cpp:2056-2065`);
///   flag-0 `PropSet` on a dictionary miss returns `TJS_E_MEMBERNOTFOUND`
///   (`tjsObject.cpp:1382-1383`), *now* the proxy falls back to the global
///   (`tjsInterCodeExec.cpp:282-287`) and writes `1` there;
/// * the second call repeats all of the above and writes `1` again.
///
/// So after two calls the global is `1` — correct TJS semantics, not a
/// counting bug. The write never lands on `d` either (`d.g` stays void,
/// since the flag-0 set missed and fell through instead of creating the
/// member).
#[test]
fn reported_repro_counts_global_once_not_twice() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { g = g + 1; } ];\n\
         d.f(); d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        1,
        "each call reads void from `d` (no global fallback on a swallowed \
         dictionary miss), computes void+1 == 1, and the flag-0 write falls \
         through to the global: the global is set to 1 on every call, so \
         after two calls it is 1, never 2"
    );
    eval_is_void(&e, "d.g");
}

/// A bare read does *not* see the global when the dictionary member is
/// absent: the dictionary swallows the miss as `TJS_S_OK` + void
/// (`tjsDictionary.cpp:739-743`), so the proxy's `TJS_E_MEMBERNOTFOUND`
/// fallback (`tjsInterCodeExec.cpp:262-268`) never runs.
///
/// This is the sharpest discriminator between "bare name == global" (would
/// return the global's `999`) and the reference model (returns void).
#[test]
fn bare_read_of_missing_dictionary_member_is_void_not_global() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 999;\n\
         var d = %[ f: function() { return g; } ];\n\
         var r = d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    eval_is_void(&e, "r");
    assert_eq!(
        eval_int(&e, "g"),
        999,
        "the read never reached the global: the dictionary miss was \
         swallowed as void before the proxy could fall back"
    );
}

/// When the dictionary *does* have the member, the bare read finds it there
/// — `this.g`, not the script global (`tjsInterCodeExec.cpp:662-667`:
/// proxy `Dispatch1 = objthis = d`; `tjsDictionary.cpp` read path).
#[test]
fn bare_read_prefers_dictionary_member_over_global() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 999;\n\
         var d = %[ g: 41, f: function() { return g + 1; } ];\n\
         var r = d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "r"),
        42,
        "the read resolves to the dictionary's own member `g`, not the \
         global `g` (bare symbol compiled to `this_proxy.g`)"
    );
    assert_eq!(eval_int(&e, "g"), 999, "the global was not consulted");
}

/// The write side of the same coin: with a pre-existing `d.g`, the flag-0
/// `PropSet` hits the dictionary and never falls back, so the global stays
/// untouched (`tjsInterCodeGen.cpp:2056-2065` compiles `this_proxy.g = v`
/// to `VM_SPD`, flag 0; the proxy only consults the global on
/// `TJS_E_MEMBERNOTFOUND`, `tjsInterCodeExec.cpp:282-287`).
#[test]
fn plain_assignment_stays_on_dictionary_when_member_exists() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ g: 5, f: function() { g = g + 1; } ];\n\
         d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "d.g"),
        6,
        "flag-0 PropSet found the dictionary member, so the increment \
         stayed on `d`"
    );
    assert_eq!(
        eval_int(&e, "g"),
        0,
        "the global was never written: no fallback occurred"
    );
}

/// The report's note that "plain-function closures are unaffected" is
/// correct: a function called as a plain statement receives the top level's
/// `this` (the global object — `ctTopLevel` executes with `objthis =
/// global`, `tjsInterCodeExec.cpp:2795-2799`), so proxy `Dispatch1 ==
/// Dispatch2 == global` and every bare read *and* write lands on the
/// global. Two calls, `g == 2`.
#[test]
fn plain_statement_function_updates_global_on_every_call() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         function f() { g = g + 1; }\n\
         f(); f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        2,
        "with `this` == global there is no dictionary in front of the \
         global, so both calls read and write it"
    );
}

/// `global` is a reserved word that compiles to the `VM_GLOBAL` opcode
/// (`tjsLex.cpp:944`, `tjsInterCodeGen.cpp:2542-2551`), bypassing the
/// this-proxy entirely. Reading and writing through it touches the real
/// global from inside a dictionary method — `g == 2` after two calls, and
/// `d.g` is never created (the write uses `VM_SPDE` because the left-hand
/// side is `T_GLOBAL`, not `T_THIS_PROXY`, `tjsInterCodeGen.cpp:2056-2065`).
#[test]
fn explicit_global_ref_in_dictionary_function_updates_global_twice() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { global.g = global.g + 1; } ];\n\
         d.f(); d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        2,
        "`global.g` addresses the global object directly, on both read \
         and write"
    );
    eval_is_void(&e, "d.g");
}

/// `var` inside a function body creates a per-call local register, not a
/// dictionary member and not a global: the function body's `EnterBlock`
/// pushes the compile-time namespace (`bison/tjs.y:355,365`), so
/// `AddLocalVariable` takes the local-register branch
/// (`tjsInterCodeGen.cpp:2861-2879`; the top level instead takes the
/// member-on-`this` branch, `tjsInterCodeGen.cpp:2880-2888`).
///
/// Consequences: each call starts fresh (`r1 == r2 == 101`, no closure
/// accumulation), the global `g` is untouched, and the dictionary never
/// gains a member `g`.
#[test]
fn var_declared_inside_function_is_call_scoped_not_closured() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { var g = 100; g = g + 1; return g; } ];\n\
         var r1 = d.f();\n\
         var r2 = d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "r1"),
        101,
        "`var g` is a local register, incremented from its initializer on \
         every call"
    );
    assert_eq!(
        eval_int(&e, "r2"),
        101,
        "locals do not persist across calls — there is no closure over \
         `var g`, each invocation re-initialises it"
    );
    assert_eq!(
        eval_int(&e, "g"),
        0,
        "the local shadows the global; the global was never touched"
    );
    eval_is_void(&e, "d.g");
}

/// Prefix `++g` inside a dictionary method compiles to `VM_INCPD`
/// (`tjsInterCodeGen.cpp:2100-2110`), which calls `Operation` — not
/// `PropGet`/`PropSet`. `tTJSDictionaryObject::Operation` *auto-creates*
/// the missing member on the dictionary before operating
/// (`tjsDictionary.cpp:764-777`), and the proxy's operation fallback
/// (`tjsInterCodeExec.cpp:430-437`) therefore never fires. The counter is
/// built on `d`, so the global stays `0` while `d.g` reaches `2`.
///
/// This is the mirror image of `g = g + 1`: increment binds to the
/// dictionary, assignment falls through to the global.
#[test]
fn prefix_increment_binds_to_dictionary_member() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { ++g; } ];\n\
         d.f(); d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        0,
        "Operation auto-created and incremented the member on `d` \
         (tjsDictionary.cpp:764-777); the global was never involved"
    );
    assert_eq!(
        eval_int(&e, "d.g"),
        2,
        "the dictionary-local counter accumulated across both calls"
    );
}

/// Postfix `g++` reads via `VM_GPD` (void from the dictionary miss) and
/// then increments via `VM_INCPD` exactly like the prefix form
/// (`tjsInterCodeGen.cpp:2114-2133`), so the observable outcome is the
/// same: `d.g == 2`, global untouched.
#[test]
fn postfix_increment_binds_to_dictionary_member() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { g++; } ];\n\
         d.f(); d.f();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        0,
        "the postfix form also operates on the dictionary's own member"
    );
    assert_eq!(
        eval_int(&e, "d.g"),
        2,
        "both calls incremented the auto-created dictionary member"
    );
}

/// What decides the scoping is the call's `objthis`, not where the
/// function was defined. Extracting the dictionary's method and calling it
/// as a plain function passes the top level's `this` (the global) instead
/// of `d` (`tjsInterCodeExec.cpp:2186-2192` picks `clo.ObjThis` only when
/// the receiver variant carries one; a bare `fn()` falls back to the
/// caller's `ra[-1]`, which at the top level is the global,
/// `tjsInterCodeExec.cpp:2795-2799`).
///
/// So the *same* function body increments the global here — and, per the
/// repro test above, sets it to `1` forever when invoked as `d.f()`.
#[test]
fn same_function_called_without_dictionary_receiver_touches_global() {
    let _lock = vm_lock();
    let e = engine();
    e.exec_script(
        "var g = 0;\n\
         var d = %[ f: function() { g = g + 1; } ];\n\
         var fn = d.f;\n\
         fn(); fn();",
        "dict_closure_scope",
    )
    .unwrap();
    assert_eq!(
        eval_int(&e, "g"),
        2,
        "with `this` == global, both calls read and write the real global"
    );
    eval_is_void(&e, "d.g");
}

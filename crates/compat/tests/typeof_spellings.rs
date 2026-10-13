//! Pins the TJS2 `typeof` spellings, which are **not** JavaScript's.
//!
//! `tTJSInterCodeContext::TypeOf` maps each variant type to a fixed name
//! (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2601-2628`): an object is
//! `"Object"` (not `"object"`), an integer is `"Integer"` and a real is
//! `"Real"` (not `"number"`), a string is `"String"` (not `"string"`), and an
//! octet is `"Octet"`. Only a member that does not exist reads back
//! `"undefined"` (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1899-1904`),
//! while a present-but-unset value is the `"void"` variant. Coding or testing
//! from JavaScript intuition silently accepts the wrong contract; this
//! fixture fails instead.
//!
//! TJS2 is also not JavaScript syntactically: there is no `{}` object literal
//! and no global `Object` constructor (both are parse/resolve errors here),
//! so object values are built with `%[]` (dictionary) or `[]` (array), and
//! `new Window()` stands in for a native instance. `typeof` of any object —
//! function values included — is still `"Object"`.

mod common;

use common::{Env, eval_then, vm_lock};
use tjs::TjsValue;

/// `typeof <expression>` as a string.
fn typeof_of(env: &Env, expression: &str) -> String {
    match env.eval_ok(&format!("typeof ({expression})")) {
        TjsValue::String(kind) => kind,
        other => panic!("typeof ({expression}) returned {other:?}, expected a string"),
    }
}

/// Reference: `tTJSInterCodeContext::TypeOf`
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2601-2628`) switches on the
/// variant type and stores a fixed name: `tvtObject` → `"Object"`,
/// `tvtString` → `"String"`, `tvtInteger` → `"Integer"`, `tvtReal` →
/// `"Real"`, `tvtOctet` → `"Octet"`, `tvtVoid` → `"void"`. `null` is an
/// object-typed variant with a null pointer
/// (`reference/cpp/core/tjs2/tjsVariant.h:552-557`), so it too is `"Object"`.
#[test]
fn typeof_uses_the_tjs_type_spellings() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dict = %[ \"a\" => 1 ]; \
         var arr = [1, 2]; \
         var noval; \
         var fn = function() {};",
    );

    assert_eq!(typeof_of(&env, "dict"), "Object", "dictionary");
    assert_eq!(typeof_of(&env, "arr"), "Object", "array");
    assert_eq!(typeof_of(&env, "null"), "Object", "null");
    assert_eq!(typeof_of(&env, "new Window()"), "Object", "native instance");
    assert_eq!(typeof_of(&env, "fn"), "Object", "function value");
    assert_eq!(typeof_of(&env, "7"), "Integer", "integer");
    assert_eq!(typeof_of(&env, "7.5"), "Real", "real");
    assert_eq!(typeof_of(&env, "\"x\""), "String", "string");
    assert_eq!(typeof_of(&env, "<% 41 42 %>"), "Octet", "octet");
    // A present-but-unset value is `void`; only an absent member is
    // `undefined` (see `undefined_is_absence_and_void_is_unset`).
    assert_eq!(typeof_of(&env, "noval"), "void", "unset variable");
}

/// Reference: `typeof` reads a member with `TJS_MEMBERMUSTEXIST`
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1119-1125`, the
/// `VM_TYPEOFD`/`VM_TYPEOFI` handlers), so a missing member reaches
/// `TypeOfMemberDirect`'s `TJS_E_MEMBERNOTFOUND` arm and becomes the string
/// `"undefined"` (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1899-1904`).
/// `void` is a different value: a declared but never-assigned variable is
/// `tvtVoid` and reports `"void"`, not `"undefined"` (JavaScript conflates
/// the two). A dictionary read *without* `typeof` still swallows the miss as
/// void (`reference/cpp/core/tjs2/tjsDictionary.cpp:739-743`), so the
/// distinction is observable only through `typeof`.
#[test]
fn undefined_is_absence_and_void_is_unset() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var dict = %[ \"a\" => 1 ];");

    // A bare `typeof <name>` for an absent global is a must-exist member read
    // and raises; `typeof global.<name>` reports the absence instead.
    assert!(
        env.eval("typeof no_such_global").is_err(),
        "a bare absent name must raise the member-not-found error"
    );
    assert_eq!(
        env.eval_ok("typeof global.no_such_global"),
        TjsValue::String("undefined".into()),
        "an absent member is `undefined`"
    );

    // A declared, never-assigned variable is `void`; the declaration must run
    // as a statement (one `eval` string cannot hold both), hence `eval_then`.
    let unset = eval_then(env.engine(), "var unset_probe;", "typeof unset_probe")
        .expect("declare then read");
    assert_eq!(unset, TjsValue::String("void".into()));

    // The dictionary miss is `undefined` through `typeof` (must-exist) but
    // void through a plain read (the dictionary swallow).
    assert_eq!(typeof_of(&env, "dict.missing"), "undefined");
    assert_eq!(env.eval_ok("dict.missing"), TjsValue::Void);
}

/// The device-log shape: a top-level `typeof <bare identifier>` for a name
/// that is not declared. Unlike `typeof obj.prop`, this **raises** in TJS2.
///
/// The `typeof` operator only takes the must-exist member path when its
/// operand is already a property access — `T_DOT`, `T_LBRACKET` or
/// `T_WITHDOT` (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:1805-1812`).
/// Any other operand, a bare symbol included, takes the "normal operation"
/// path (`:1815-1839`): the symbol is read as an ordinary variable first and
/// the result is typed afterwards. A symbol that is not a local is rewritten
/// to `this.<name>` / `global.<name>`
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:2320-2345`) and read with
/// `VM_GPD` (`:2039-2049`), i.e. `GetPropertyDirect` with flags `0`
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1166-1167`, `:1400-1428`).
/// The global object is a `tTJSCustomObject`, whose `PropGet` returns
/// `TJS_E_MEMBERNOTFOUND` for an absent member
/// (`reference/cpp/core/tjs2/tjsObject.cpp:1276-1310`), and
/// `TJSThrowFrom_tjs_error` turns that into `Member "<name>" does not exist`
/// (`reference/cpp/core/tjs2/tjsError.cpp:254-268`;
/// `reference/cpp/core/tjs2/tjsError.h:264`). Only the property-access form
/// reaches `TypeOfMemberDirect`'s `"undefined"` arm
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1865-1904`), which is what
/// the reference's own `motionplayer` uses to probe a class member
/// (`reference/cpp/plugins/motionplayer/main.cpp:505`,
/// `typeof Motion.Player.useD3D`). The device log's bare `typeof Motion`
/// therefore raises in the reference too; the top-level script catches it and
/// keeps running, which is why the dump appears without aborting the title.
#[test]
fn bare_undeclared_global_typeof_raises() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env.error("typeof SomeAbsentGlobal");
    assert!(
        error.contains("SomeAbsentGlobal") && error.contains("does not exist"),
        "a bare absent identifier must raise the reference member-not-found error, got: {error}"
    );

    // The property-access spelling is the one that reports absence instead.
    assert_eq!(
        env.eval_ok("typeof global.SomeAbsentGlobal"),
        TjsValue::String("undefined".into())
    );
}

/// TJS2 is not JavaScript syntactically either: `{}` is not an object literal
/// (the parser reads it as a block and rejects the expression) and there is no
/// global `Object` constructor (a bare `new Object()` is a member lookup that
/// raises). Pinning both errors keeps a JavaScript-flavoured rewrite from
/// quietly swapping the object-construction idiom, while the `%[]`/`[]`
/// spellings above stay the way to produce TJS objects.
#[test]
fn javascript_object_spellings_are_not_tjs() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(
        env.eval("typeof {}").is_err(),
        "`{{}}` is not a TJS object literal"
    );
    assert!(
        env.eval("new Object()").is_err(),
        "`Object` is not a global constructor"
    );
}

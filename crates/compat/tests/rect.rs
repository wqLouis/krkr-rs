//! Integration tests for the `Rect` native class.
//!
//! Every test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`], because the engine → context binding is
//! process-wide.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// Build a VM with the full compat surface installed and run `f` against it.
///
/// The engine is a local (never moved after install), so the binding's stored
/// address stays valid; `host` is declared after the engine and therefore
/// drops first, releasing the binding while the VM is still alive.
fn with_vm(f: impl FnOnce(&Tjs2Engine)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install compat surface");
    f(&engine);
}

fn eval_i(e: &Tjs2Engine, expr: &str) -> i64 {
    match e.eval(expr, "test").expect("eval succeeds") {
        TjsValue::Integer(v) => v,
        other => panic!("expected Integer for {expr:?}, got {other:?}"),
    }
}

fn run(e: &Tjs2Engine, script: &str) {
    e.exec_script(script, "test").expect("exec succeeds");
}

#[test]
fn default_constructor_is_empty_and_zero_sized() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(eval_i(e, "(new Rect()).isEmpty()"), 1);
        assert_eq!(eval_i(e, "(new Rect()).width"), 0);
        assert_eq!(eval_i(e, "(new Rect()).height"), 0);
        assert_eq!(eval_i(e, "(new Rect()).left"), 0);
        assert_eq!(eval_i(e, "(new Rect()).right"), 0);
    });
}

#[test]
fn four_argument_constructor_sets_the_edges() {
    let _lock = vm_lock();
    with_vm(|e| {
        // Reference order is `Rect(left, top, right, bottom)`.
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).left"), 10);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).top"), 20);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).right"), 50);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).bottom"), 80);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).width"), 40);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).height"), 60);
        assert_eq!(eval_i(e, "(new Rect(10, 20, 50, 80)).isEmpty()"), 0);
    });
}

#[test]
fn copy_constructor_and_equal() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(e, "var a = new Rect(1, 2, 3, 4); var b = new Rect(a);");
        assert_eq!(eval_i(e, "b.left"), 1);
        assert_eq!(eval_i(e, "b.top"), 2);
        assert_eq!(eval_i(e, "b.right"), 3);
        assert_eq!(eval_i(e, "b.bottom"), 4);
        assert_eq!(eval_i(e, "a.equal(b)"), 1);

        // The copy is independent of the source.
        run(e, "b.left = 9;");
        assert_eq!(eval_i(e, "a.equal(b)"), 0);
        assert_eq!(eval_i(e, "a.left"), 1);
    });
}

#[test]
fn size_offset_and_move_keep_the_reference_semantics() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(e, "var r = new Rect(0, 0, 10, 10); r.setSize(4, 5);");
        assert_eq!(eval_i(e, "r.width"), 4);
        assert_eq!(eval_i(e, "r.height"), 5);
        assert_eq!(eval_i(e, "r.right"), 4);
        assert_eq!(eval_i(e, "r.bottom"), 5);

        run(e, "r.setOffset(100, 200);");
        assert_eq!(eval_i(e, "r.left"), 100);
        assert_eq!(eval_i(e, "r.top"), 200);
        assert_eq!(eval_i(e, "r.width"), 4);
        assert_eq!(eval_i(e, "r.height"), 5);

        run(e, "r.addOffset(-100, -200);");
        assert_eq!(eval_i(e, "r.left"), 0);
        assert_eq!(eval_i(e, "r.top"), 0);
        assert_eq!(eval_i(e, "r.right"), 4);
        assert_eq!(eval_i(e, "r.bottom"), 5);
    });
}

#[test]
fn set_clear_and_property_writes() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(e, "var r = new Rect(); r.set(1, 2, 3, 4);");
        assert_eq!(eval_i(e, "r.left"), 1);
        assert_eq!(eval_i(e, "r.top"), 2);
        assert_eq!(eval_i(e, "r.right"), 3);
        assert_eq!(eval_i(e, "r.bottom"), 4);

        // `width`/`height` setters keep `left`/`top` fixed.
        run(e, "r.width = 10; r.height = 20;");
        assert_eq!(eval_i(e, "r.right"), 11);
        assert_eq!(eval_i(e, "r.bottom"), 22);

        run(e, "r.clear();");
        assert_eq!(eval_i(e, "r.isEmpty()"), 1);
        assert_eq!(eval_i(e, "r.left"), 0);
        assert_eq!(eval_i(e, "r.bottom"), 0);
    });
}

#[test]
fn clip_intersects_and_union() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(
            e,
            "var a = new Rect(0, 0, 10, 10); var b = new Rect(5, 5, 15, 15);",
        );
        assert_eq!(eval_i(e, "a.intersects(b)"), 1);
        assert_eq!(eval_i(e, "a.clip(b)"), 1);
        assert_eq!(eval_i(e, "a.left"), 5);
        assert_eq!(eval_i(e, "a.top"), 5);
        assert_eq!(eval_i(e, "a.right"), 10);
        assert_eq!(eval_i(e, "a.bottom"), 10);

        // A disjoint clip leaves the receiver unchanged and returns false.
        run(
            e,
            "var c = new Rect(20, 20, 30, 30); var d = new Rect(0, 0, 10, 10);",
        );
        assert_eq!(eval_i(e, "d.intersects(c)"), 0);
        assert_eq!(eval_i(e, "d.clip(c)"), 0);
        assert_eq!(eval_i(e, "d.right"), 10);

        // Union grows to cover both rectangles.
        assert_eq!(eval_i(e, "d.union(b)"), 1);
        assert_eq!(eval_i(e, "d.left"), 0);
        assert_eq!(eval_i(e, "d.right"), 15);
        assert_eq!(eval_i(e, "d.bottom"), 15);
    });
}

#[test]
fn included_and_included_pos_use_half_open_edges() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(
            e,
            "var a = new Rect(0, 0, 10, 10); \
             var b = new Rect(5, 5, 15, 15); \
             var outer = new Rect(-1, -1, 11, 11);",
        );
        assert_eq!(eval_i(e, "a.included(outer)"), 1);
        assert_eq!(eval_i(e, "b.included(a)"), 0);
        assert_eq!(eval_i(e, "a.includedPos(0, 0)"), 1);
        assert_eq!(eval_i(e, "a.includedPos(9, 9)"), 1);
        // Right/bottom edges are exclusive.
        assert_eq!(eval_i(e, "a.includedPos(10, 9)"), 0);
        assert_eq!(eval_i(e, "a.includedPos(9, 10)"), 0);
    });
}

#[test]
fn non_object_argument_is_a_void_no_op() {
    let _lock = vm_lock();
    with_vm(|e| {
        run(e, "var a = new Rect(1, 2, 3, 4);");
        // The reference only fills the result when the argument is an object.
        assert_eq!(e.eval("a.clip(5)", "test").unwrap(), TjsValue::Void);
        assert_eq!(e.eval("a.intersects('x')", "test").unwrap(), TjsValue::Void);
        assert_eq!(eval_i(e, "a.left"), 1);
    });
}

#[test]
fn argument_counts_other_than_zero_one_or_four_are_ignored() {
    let _lock = vm_lock();
    with_vm(|e| {
        // The reference `Construct` only handles 1 and 4 parameters; other
        // counts leave the fresh all-zero payload.
        assert_eq!(eval_i(e, "(new Rect(1, 2, 3)).left"), 0);
        assert_eq!(eval_i(e, "(new Rect(1, 2, 3, 4, 5)).right"), 0);
    });
}

#[test]
fn invalid_arguments_are_typed_errors() {
    let _lock = vm_lock();
    with_vm(|e| {
        // A single non-object parameter is not a valid copy source.
        assert!(e.eval("new Rect(5)", "test").is_err());
        assert!(e.eval("new Rect(%[])", "test").is_err());
        // Missing required parameters.
        assert!(e.eval("(new Rect()).setSize(1)", "test").is_err());
        assert!(e.eval("(new Rect()).setOffset(1)", "test").is_err());
        assert!(e.eval("(new Rect()).addOffset(1)", "test").is_err());
        assert!(e.eval("(new Rect()).set(1, 2, 3)", "test").is_err());
        assert!(e.eval("(new Rect()).includedPos(1)", "test").is_err());
        // An object that is not a Rect is rejected by the binary methods.
        assert!(e.eval("(new Rect()).clip(%[])", "test").is_err());
    });
}

#[test]
fn native_array_is_a_read_only_pointer_value() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert!(matches!(
            e.eval("(new Rect(1, 2, 3, 4)).nativeArray", "test")
                .unwrap(),
            TjsValue::Integer(_)
        ));
        run(e, "var r = new Rect(1, 2, 3, 4);");
        assert!(e.exec_script("r.nativeArray = 0;", "test").is_err());
    });
}

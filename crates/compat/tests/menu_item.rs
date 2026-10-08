//! Integration tests for the `MenuItem` native class.
//!
//! The menu tree lives in the per-binding `CompatServices` dispatch state, so
//! each test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`] (the engine → context binding is process-wide).
//! The tests exercise the script-visible surface: construction, the tree
//! operations, the item flags (including the radio group), traversal, click
//! dispatch and the static shortcut tables. The two members that need a
//! platform menu (`HMENU`, `popup`) are asserted to raise the explicit typed
//! error rather than silently default.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so that (a) the
/// binding's stored engine address stays valid when `Env` is moved (the `Box`
/// keeps the `Tjs2Engine` on the heap) and (b) the host's `Drop` removes the
/// process-wide binding before the VM is destroyed.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "menu-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "menu-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

#[test]
fn tree_links_and_default_flags() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var root = new MenuItem(); \
         var a = new MenuItem(null, 'A'); \
         var b = new MenuItem(null, 'B'); \
         root.add(a); \
         root.insert(b, 0);",
    );

    // Parent/root/index and the `children` array hand back the real objects.
    assert_eq!(env.eval_int("b.parent === root"), 1);
    assert_eq!(env.eval_int("b.root === root"), 1);
    assert_eq!(env.eval_int("a.parent === root"), 1);
    assert_eq!(env.eval_int("b.index"), 0);
    assert_eq!(env.eval_int("a.index"), 1);
    assert_eq!(env.eval_int("root.children.length"), 2);
    assert_eq!(env.eval_int("root.children[0] === b"), 1);
    assert_eq!(env.eval_int("root.children[1] === a"), 1);
    assert_eq!(env.eval_int("root.parent === null"), 1);
    assert_eq!(env.eval_int("root.root === root"), 1);

    // Reference defaults.
    assert_eq!(env.eval_int("a.enabled"), 1);
    assert_eq!(env.eval_int("a.visible"), 1);
    assert_eq!(env.eval_int("a.checked"), 0);
    assert_eq!(env.eval_int("a.radio"), 0);
    assert_eq!(env.eval_int("a.group"), 0);
    assert_eq!(env.eval_str("a.caption"), "A");
    assert_eq!(env.eval_str("a.shortcut"), "");

    // Caption from the constructor's second (non-object) argument.
    env.run("var c = new MenuItem(null, 'Cap');");
    assert_eq!(env.eval_str("c.caption"), "Cap");
}

#[test]
fn checked_radio_group_clears_siblings() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var root = new MenuItem(); \
         var a = new MenuItem(null, 'A'); \
         var b = new MenuItem(null, 'B'); \
         var c = new MenuItem(null, 'C'); \
         root.add(a); root.add(b); root.add(c); \
         a.radio = true; b.radio = true; c.radio = true; \
         a.group = 2; b.group = 2; c.group = 3; \
         b.checked = true;",
    );
    assert_eq!(env.eval_int("b.checked"), 1);

    // Checking another member of the same group clears `b`; a different group
    // is untouched.
    env.run("a.checked = true;");
    assert_eq!(env.eval_int("a.checked"), 1);
    assert_eq!(env.eval_int("b.checked"), 0);
    env.run("c.checked = true;");
    assert_eq!(env.eval_int("c.checked"), 1);
    assert_eq!(env.eval_int("a.checked"), 1);
}

#[test]
fn insert_remove_and_index_move() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var root = new MenuItem(); \
         var a = new MenuItem(null, 'A'); \
         var b = new MenuItem(null, 'B'); \
         var c = new MenuItem(null, 'C'); \
         root.add(a); root.add(b); root.add(c);",
    );
    assert_eq!(env.eval_int("root.children[0] === a"), 1);

    env.run("a.index = 2;");
    assert_eq!(env.eval_int("a.index"), 2);
    assert_eq!(env.eval_int("root.children[0] === b"), 1);
    assert_eq!(env.eval_int("root.children[2] === a"), 1);

    env.run("root.remove(b);");
    assert_eq!(env.eval_int("root.children.length"), 2);
    assert_eq!(env.eval_int("b.parent === null"), 1);

    // Removing a non-child is a no-op.
    env.run("root.remove(b);");
    assert_eq!(env.eval_int("root.children.length"), 2);
}

#[test]
fn children_array_is_cached_and_refreshed() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var root = new MenuItem(); \
         var a = new MenuItem(null, 'A'); \
         root.add(a); \
         var kids = root.children;",
    );
    assert_eq!(env.eval_int("kids.length"), 1);
    assert_eq!(env.eval_int("root.children === root.children"), 1);

    env.run("root.add(new MenuItem(null, 'B'));");
    // The getter refreshes the *same* array object (reference
    // `GetChildrenArrayNoAddRef`), so a held reference sees the new child.
    assert_eq!(env.eval_int("root.children === kids"), 1);
    assert_eq!(env.eval_int("kids.length"), 2);
}

#[test]
fn fire_click_honours_enabled_and_script_on_click() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; \
         var m = new MenuItem(null, 'Open'); \
         m.onClick = function() { n += 1; }; \
         m.shortcut = 'Ctrl+O'; \
         m.enabled = false; \
         m.fireClick(); \
         m.enabled = true; \
         m.fireClick();",
    );
    assert_eq!(env.eval_str("m.caption"), "Open");
    assert_eq!(env.eval_str("m.shortcut"), "Ctrl+O");
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn fire_click_is_blocked_by_a_disabled_ancestor() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; \
         var root = new MenuItem(); \
         var child = new MenuItem(null, 'Child'); \
         child.onClick = function() { n += 1; }; \
         root.add(child); \
         root.enabled = false; \
         child.fireClick(); \
         root.enabled = true; \
         child.fireClick();",
    );
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn action_owner_receives_the_on_click_event() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var fired = 0; \
         var seen = ''; \
         var owner = %[ action: function(ev) { fired = fired + 1; seen = '' + ev.type; } ]; \
         var m = new MenuItem(owner, 'Dispatch'); \
         m.fireClick();",
    );
    assert_eq!(env.eval_int("fired"), 1);
    assert_eq!(env.eval_str("seen"), "onClick");

    // A disabled item never reaches the action owner.
    env.run(
        "var fired2 = 0; \
         var owner2 = %[ action: function(ev) { fired2 = fired2 + 1; } ]; \
         var m2 = new MenuItem(owner2, 'Disabled'); \
         m2.enabled = false; \
         m2.fireClick();",
    );
    assert_eq!(env.eval_int("fired2"), 0);
}

#[test]
fn window_property_returns_the_attached_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = %[id: 7]; \
         var root = new MenuItem(null, w); \
         var detached = new MenuItem(null, 'x');",
    );
    assert_eq!(env.eval_int("root.window === w"), 1);
    assert_eq!(env.eval_int("detached.window === null"), 1);
}

#[test]
fn shortcut_tables_round_trip_and_are_read_only() {
    let _lock = vm_lock();
    let env = Env::new();

    // Reference entries survive on both static tables.
    assert_eq!(env.eval_int("MenuItem.textToKeycode['bksp']"), 8);
    assert_eq!(env.eval_str("MenuItem.keycodeToText[8]"), "BkSp");
    for (key, code) in [
        ("a", 65),
        ("z", 90),
        ("f1", 112),
        ("f12", 123),
        ("enter", 13),
        ("esc", 27),
        ("pgup", 33),
        ("pgdn", 34),
    ] {
        assert_eq!(
            env.eval_int(&format!("MenuItem.textToKeycode['{key}']")),
            code,
            "textToKeycode[{key}]"
        );
    }
    assert_eq!(env.eval_str("MenuItem.keycodeToText[65]"), "A");
    assert_eq!(env.eval_str("MenuItem.keycodeToText[123]"), "F12");

    // Read-only static properties.
    assert!(env.eval("MenuItem.textToKeycode = %[]").is_err());
    assert!(env.eval("MenuItem.keycodeToText = []").is_err());
}

#[test]
fn hmenu_and_popup_raise_the_typed_platform_error() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var m = new MenuItem();");

    let hmenu = env.error("m.HMENU");
    assert!(
        hmenu.contains("platform menu"),
        "HMENU error should name the deferral, got {hmenu:?}"
    );
    let popup = env.error("m.popup(0, 10, 20)");
    assert!(
        popup.contains("platform menu"),
        "popup error should name the deferral, got {popup:?}"
    );
    // Reference arity check comes first.
    assert!(env.error("m.popup()").contains("requires flags"));
}

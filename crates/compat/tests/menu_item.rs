//! Integration tests for the `MenuItem` native class.
//!
//! The menu tree lives in the per-binding `CompatServices` dispatch state, so
//! each test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`] (the engine → context binding is process-wide).
//! The tests exercise the script-visible surface: construction, the tree
//! operations, the item flags (including the radio group), traversal, click
//! dispatch — gated on an ancestor `Window` and that window's `visible`, like
//! the reference `OnClick` — and the static shortcut tables. The two members
//! that the desktop build backs with a platform menu (`HMENU`, `popup`) report
//! reference-shaped values from the logical tree: `HMENU` is `0` (the reference
//! `GetMenuItemHandleForPlugin` returns `nullptr`) and `popup` returns the
//! success value `1`.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached L2 scene.
///
/// The engine is boxed and the host is declared **before** it so that (a) the
/// binding's stored engine address stays valid when `Env` is moved (the `Box`
/// keeps the `Tjs2Engine` on the heap) and (b) the host's `Drop` removes the
/// process-wide binding before the VM is destroyed. The scene is boxed and
/// declared last so the host's raw pointer to it stays valid for the whole
/// lifetime of the binding; it backs the native `Window` the click-dispatch
/// tests attach to their menu trees.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the binding installed below.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
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

    // Reference: `tTJSNI_MenuItem::SetIndex` is an empty body on this
    // platform — it never reorders the parent's children, and the getter
    // keeps reporting the actual position
    // (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:209-214`).
    env.run("a.index = 2;");
    assert_eq!(env.eval_int("a.index"), 0);
    assert_eq!(env.eval_int("root.children[0] === a"), 1);
    assert_eq!(env.eval_int("root.children[1] === b"), 1);
    assert_eq!(env.eval_int("root.children[2] === c"), 1);
    assert_eq!(env.eval_int("root.children.length"), 3);

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

/// Reference: `OnClick` first checks the enabled chain and then requires a
/// `Window` somewhere in the ancestor chain whose `CanDeliverEvents()` holds
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:196-213`), so the item is
/// attached to a root that carries a native `Window` (the scene starts its
/// windows visible) before the click is expected to deliver.
#[test]
fn fire_click_honours_enabled_and_script_on_click() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new MenuItem(null, w); \
         var n = 0; \
         var m = new MenuItem(null, 'Open'); \
         m.onClick = function() { n += 1; }; \
         m.shortcut = 'Ctrl+O'; \
         root.add(m); \
         m.enabled = false; \
         m.fireClick(); \
         m.enabled = true; \
         m.fireClick();",
    );
    assert_eq!(env.eval_str("m.caption"), "Open");
    assert_eq!(env.eval_str("m.shortcut"), "Ctrl+O");
    assert_eq!(env.eval_int("n"), 1);
}

/// Reference: a disabled item (or one under a disabled ancestor) returns
/// before the event is posted (`reference/cpp/core/visual/MenuItemIntf.cpp:196-199`),
/// *given* the window the reference `OnClick` additionally demands — the
/// tree below carries its `Window` on the root ancestor
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:200-211`).
#[test]
fn fire_click_is_blocked_by_a_disabled_ancestor() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var n = 0; \
         var root = new MenuItem(null, w); \
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

/// Reference: `OnClick` walks `this` → `Parent` looking for a `Window` and
/// returns silently when none carries one (`reference/cpp/core/visual/MenuItemIntf.cpp:202-209`);
/// when one does, it must itself be deliverable — visible
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:210-211`). So a window-less
/// tree never delivers, an attached-but-invisible window never delivers,
/// and making the window visible lets the same click through.
#[test]
fn fire_click_requires_a_visible_ancestor_window() {
    let _lock = vm_lock();
    let env = Env::new();

    // No window anywhere in the chain: the enabled click is dropped.
    env.run(
        "var n = 0; \
         var plain = new MenuItem(null, 'Plain'); \
         plain.onClick = function() { n += 1; }; \
         plain.fireClick();",
    );
    assert_eq!(env.eval_int("n"), 0);

    // Attaching a window is not enough while it is invisible.
    env.run(
        "var w = new Window(); \
         var root = new MenuItem(null, w); \
         root.add(plain); \
         w.visible = false; \
         plain.fireClick();",
    );
    assert_eq!(env.eval_int("n"), 0);

    // The visible window finally delivers.
    env.run("w.visible = true; plain.fireClick();");
    assert_eq!(env.eval_int("n"), 1);
}

/// Reference: the click reaches the object's `onClick`, whose native default
/// dispatches `owner.action(event)` — so the action owner observes the
/// `onClick` event when the item is deliverable
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:196-213`,
/// `reference/cpp/core/visual/MenuItemIntf.cpp:305-316`). The item is built
/// on a root that carries a native `Window`, as the reference requires.
#[test]
fn action_owner_receives_the_on_click_event() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new MenuItem(null, w); \
         var fired = 0; \
         var seen = ''; \
         var owner = %[ action: function(ev) { fired = fired + 1; seen = '' + ev.type; } ]; \
         var m = new MenuItem(owner, w); \
         root.add(m); \
         m.fireClick();",
    );
    assert_eq!(env.eval_int("fired"), 1);
    assert_eq!(env.eval_str("seen"), "onClick");

    // A disabled item never reaches the action owner.
    env.run(
        "var fired2 = 0; \
         var owner2 = %[ action: function(ev) { fired2 = fired2 + 1; } ]; \
         var m2 = new MenuItem(owner2, w); \
         root.add(m2); \
         m2.enabled = false; \
         m2.fireClick();",
    );
    assert_eq!(env.eval_int("fired2"), 0);
}

/// Reference: the constructor takes a native `Window` (or nothing) as its
/// second argument and throws `TVPSpecifyWindow` for any other object
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:42-73`); `window` then hands
/// back exactly that object, and an item built without one reports `null`.
#[test]
fn window_property_returns_the_attached_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new MenuItem(null, w); \
         var detached = new MenuItem(null, 'x');",
    );
    assert_eq!(env.eval_int("root.window === w"), 1);
    assert_eq!(env.eval_int("detached.window === null"), 1);

    // A plain object is not a `Window`: the reference raises TVPSpecifyWindow
    // (`reference/cpp/core/visual/MenuItemIntf.cpp:61-70`).
    let error = env.error("new MenuItem(null, %[id: 7])");
    assert!(
        error.contains("Window"),
        "the constructor must reject a non-Window object, got {error:?}"
    );
}

/// Reference: `CreateShortCutKeyCodeTable` builds both static tables with
/// the `MapVirtualKey` loop `#if 0`-disabled, so exactly the three
/// unconditional entries `BkSp`/`PgUp`/`PgDn` survive
/// (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:363-395`). Every other
/// key/code pair is absent: a missing dictionary member reads back as void
/// (`reference/cpp/core/tjs2/tjsDictionary.cpp:732-745`) and an out-of-range
/// array index likewise (`reference/cpp/core/tjs2/tjsArray.cpp:1383-1390`).
#[test]
fn shortcut_tables_round_trip_and_are_read_only() {
    let _lock = vm_lock();
    let env = Env::new();

    // The three unconditional entries survive on both static tables.
    assert_eq!(env.eval_int("MenuItem.textToKeycode['bksp']"), 8);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['pgup']"), 33);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['pgdn']"), 34);
    assert_eq!(env.eval_str("MenuItem.keycodeToText[8]"), "BkSp");
    assert_eq!(env.eval_str("MenuItem.keycodeToText[33]"), "PgUp");
    assert_eq!(env.eval_str("MenuItem.keycodeToText[34]"), "PgDn");
    // The value list is a sparse array sized by the highest entry (34 + 1).
    assert_eq!(env.eval_int("MenuItem.keycodeToText.count"), 35);

    // Nothing else exists — the `MapVirtualKey`/`GetKeyNameText` loop that
    // used to fill these slots is `#if 0` upstream.
    for key in ["a", "z", "enter", "esc", "f1", "f12"] {
        assert_eq!(
            env.eval(&format!("MenuItem.textToKeycode['{key}']")),
            Ok(TjsValue::Void),
            "textToKeycode[{key}] must not exist"
        );
    }
    for code in [13, 27, 65, 112, 123] {
        assert_eq!(
            env.eval(&format!("MenuItem.keycodeToText[{code}]")),
            Ok(TjsValue::Void),
            "keycodeToText[{code}] must not exist"
        );
    }

    // Read-only static properties.
    assert!(env.eval("MenuItem.textToKeycode = %[]").is_err());
    assert!(env.eval("MenuItem.keycodeToText = []").is_err());
}

#[test]
fn hmenu_is_zero_and_popup_reports_success() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var m = new MenuItem(); var other = new MenuItem();");

    // Reference `HMENU` getter: the property casts the native menu handle to
    // an integer (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:409-413`).
    // `GetMenuItemHandleForPlugin` returns `nullptr` unconditionally
    // (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:193-196`), so every
    // item reports `0`.
    assert_eq!(env.eval_int("m.HMENU"), 0);
    assert_eq!(env.eval_int("other.HMENU"), 0);

    // Reference `TrackPopup` returns `1`
    // (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:309-315`) and the
    // `popup` native method returns that result
    // (`reference/cpp/core/visual/MenuItemIntf.cpp:285-299`).
    assert_eq!(env.eval_int("m.popup(0, 10, 20)"), 1);

    // Reference `popup` arity check comes first
    // (`reference/cpp/core/visual/MenuItemIntf.cpp:289-290`).
    assert!(env.error("m.popup()").contains("requires flags"));
}

#[test]
fn popup_coerces_its_arguments_before_returning_success() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var m = new MenuItem();");
    // `tjs_uint32 flags = (tTVInteger)*param[0]; tjs_int x = *param[1];
    // tjs_int y = *param[2];` (`MenuItemIntf.cpp:290-292`): each `AsInteger`
    // conversion raises `TJSThrowVariantConvertError` on an object or `null`
    // before `TrackPopup` runs.
    assert!(env.error("m.popup(%[], 10, 20)").contains("object"));
    assert!(env.error("m.popup(0, null, 20)").contains("null"));
    assert!(env.error("m.popup(0, 10, %[])").contains("object"));
    // The success value is still `1` for coercible arguments.
    assert_eq!(env.eval_int("m.popup(0, 10, 20)"), 1);
}

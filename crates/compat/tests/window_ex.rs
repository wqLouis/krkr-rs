//! Reference-behaviour tests for the `window_ex` plugin (the KiriKiri
//! `windowEx.dll` emulation).
//!
//! Every test states the **reference expectation** with a
//! `reference/cpp/**` citation above it and asserts that expectation against
//! the observed script-visible behavior, the same way
//! `crates/compat/tests/timer_behavior.rs` does. Where the two differ the
//! assertion fails with the observed value, so `cargo test -p compat --test
//! window_ex` doubles as the observed-vs-expected report.
//!
//! Ground rules applied here:
//! * The vendored TJS2 VM and `ncbind.hpp` (`reference/cpp/core/**`) are
//!   reference code: argument-count enforcement, variant coercion and member
//!   lookup order are taken from them; divergences are attributed to
//!   `crates/compat/src/natives/plugins/window_ex/**`.
//! * The vendored `windowEx.cpp` stubs every Win32 body (minimize, console
//!   placement, …). Where the body still contains *logic* — argument
//!   validation, coercion, dictionary lookups, flag returns — that logic is
//!   pinned exactly; where the body is a bare `return TJS_S_OK` with no
//!   observable effect (e.g. `getDisplayMonitors`), the port's documented
//!   host seam (typed D11 error until a host provider is installed) is
//!   asserted instead and noted in the test.
//! * Each test builds its own VM + [`CompatHost`] under the shared
//!   [`common::vm_lock`] (the engine → context binding is process-wide). A
//!   caller-owned L2 [`Scene`] is attached so `Window`/`Layer` instances have
//!   a real backing store.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "window-ex-test";

/// A bound VM + host + attached scene.
///
/// The host is declared before the engine and scene so it drops first,
/// releasing the retained script objects while the VM is still alive.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host (it is declared after
        // `host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
        }
    }

    fn host(&self) -> &CompatHost {
        &self.host
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, SCRIPT)
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_err(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should raise"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Run `assign` (a statement list), then read `read` back through its
    /// getter.
    ///
    /// [`Self::eval`] compiles a **single expression**: the reference's
    /// `EvalExpression` runs the lexer in expression mode, which appends a
    /// `;` and, when a result is needed, returns the first token as
    /// `return` (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:198-243`,
    /// `reference/cpp/core/tjs2/tjsLex.cpp:1006-1008,1676-1679`). Mixing an
    /// assignment and a following read in one string therefore yields the
    /// assignment's value and leaves the read unreachable; only separate
    /// executions reach the getter.
    fn assign_then(&self, assign: &str, read: &str) -> i64 {
        self.run(assign);
        self.eval_int(read)
    }

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn eval_void(&self, expression: &str) {
        match self.eval_ok(expression) {
            TjsValue::Void => {}
            other => panic!("{expression:?} returned {other:?}, expected void"),
        }
    }
}

// ---------------------------------------------------------------------------
// Window: argument counts and inert actions
// ---------------------------------------------------------------------------

/// Reference: `getWindowRect` clears the result and rebuilds it from the
/// `left`/`top`/`width`/`height` integer reads as `{x, y, w, h}`
/// (`reference/cpp/plugins/windowEx.cpp:399-427`); `getClientRect` and
/// `getNormalRect` are plain aliases of that body (`windowEx.cpp:428-443`).
/// The coordinates come back through `ncbPropAccessor::getIntValue`, i.e. as
/// *integers* (`reference/cpp/core/plugin/ncbind.hpp:689-695`).
#[test]
fn window_rect_members_mirror_the_window_box() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.left = 10; w.top = 20; w.width = 300; w.height = 200; \
         var r = w.getWindowRect(); var c = w.getClientRect(); var n = w.getNormalRect();",
    );
    for (name, expr) in [("x", "r.x"), ("y", "r.y"), ("w", "r.w"), ("h", "r.h")] {
        assert!(
            matches!(env.eval_ok(expr), TjsValue::Integer(_)),
            "{name} ({expr}) is not an integer"
        );
    }
    assert_eq!(env.eval_int("r.x"), 10, "x");
    assert_eq!(env.eval_int("r.y"), 20, "y");
    assert_eq!(env.eval_int("r.w"), 300, "w");
    assert_eq!(env.eval_int("r.h"), 200, "h");
    assert_eq!(
        env.eval_int("(c.x == r.x) + (c.y == r.y) + (c.w == r.w) + (c.h == r.h)"),
        4
    );
    assert_eq!(
        env.eval_int("(n.x == r.x) + (n.y == r.y) + (n.w == r.w) + (n.h == r.h)"),
        4
    );
}

/// Reference: `nonClientHitTest` starts with
/// `if(n < 2) return TJS_E_BADPARAMCOUNT;` and returns `TJS_S_OK` without
/// writing a result when both coordinates are present
/// (`reference/cpp/plugins/windowEx.cpp:586-594`), attached at
/// `windowEx.cpp:985`.
#[test]
fn nc_hit_test_requires_two_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    assert!(env.eval("w.ncHitTest()").is_err(), "0 args must raise");
    assert!(env.eval("w.ncHitTest(1)").is_err(), "1 arg must raise");
    env.eval_void("w.ncHitTest(0, 0)");
    // Extra arguments are not validated by the reference body.
    env.eval_void("w.ncHitTest(1, 2, 3)");
}

/// Reference: `focusMenuByKey` unconditionally reads its first parameter
/// (`(*p)->AsInteger()` at `reference/cpp/plugins/windowEx.cpp:379-383`),
/// so a call without an argument cannot be honoured; with an argument it
/// returns `TJS_S_OK` (`windowEx.cpp:986`).
#[test]
fn focus_menu_by_key_reads_one_argument() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    env.eval_void("w.focusMenuByKey(0)");
    env.eval_void("w.focusMenuByKey('2')");
    assert!(
        env.eval("w.focusMenuByKey()").is_err(),
        "the reference reads p[0] unconditionally"
    );
}

/// Reference: `minimize`, `maximize`, `showRestore`, `resetWindowIcon`,
/// `setWindowIcon`, `bringTo` and `sendToBack` are attached as raw callbacks
/// (`reference/cpp/plugins/windowEx.cpp:960-989`) whose bodies return
/// `TJS_S_OK` without reading `n`/`p` or writing a result
/// (`windowEx.cpp:364-397,623-650`), so any argument count is accepted and
/// the call reads back as void.
#[test]
fn inert_window_actions_accept_any_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); var other = new Window(); \
         w.minimize(); \
         w.maximize('x', 1); \
         w.showRestore(); \
         w.resetWindowIcon(1, 2, 3); \
         w.setWindowIcon('icon.ico'); \
         w.bringTo('top'); \
         w.bringTo(other); \
         w.bringTo(); \
         w.sendToBack();",
    );
    assert_eq!(env.eval_ok("w.minimize()"), TjsValue::Void);
    assert_eq!(env.eval_ok("w.sendToBack()"), TjsValue::Void);
}

// ---------------------------------------------------------------------------
// Window: setMessageHook
// ---------------------------------------------------------------------------

/// Reference: `setMessageHook` is `if(self == nullptr) ACCESSDENYED;` then
/// `bool on = (n >= 1) && !!p[0]->AsInteger();` (`windowEx.cpp:596-601`).
/// With a second argument it validates the message number
/// (`windowEx.cpp:602-612`) and calls `setMessageHookOnel(on, num)`, which
/// **returns the all-hook flag unchanged and stores nothing**
/// (`windowEx.cpp:747`, call at `windowEx.cpp:613`). Without a second
/// argument it calls `setMessageHookAll(on)`, which assigns the flag and
/// returns it (`windowEx.cpp:749`, call at `windowEx.cpp:615`). The flag
/// starts out `false` (`windowEx.cpp:717`). `*r = ret` writes the bool as an
/// integer (`reference/cpp/core/tjs2/tjsVariant.cpp:630-635`).
#[test]
fn set_message_hook_returns_the_all_hook_flag() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    // No arguments: on = false, the all-flag is assigned false → 0.
    assert_eq!(env.eval_int("w.setMessageHook()"), 0);
    // The all-form assigns and returns.
    assert_eq!(env.eval_int("w.setMessageHook(1)"), 1);
    // The one-form returns the *all-hook* flag (still 1) and stores nothing.
    assert_eq!(env.eval_int("w.setMessageHook(0, 'WM_MOUSEMOVE')"), 1);
    // Turning the all-hook off…
    assert_eq!(env.eval_int("w.setMessageHook(0)"), 0);
    // …means the one-form now reports 0 — even with `on = 1`.
    assert_eq!(env.eval_int("w.setMessageHook(1, 'WM_MOUSEMOVE')"), 0);
    // The one-form never set anything: the all-form still reads false.
    assert_eq!(env.eval_int("w.setMessageHook()"), 0);
    // A numeric second argument takes the same path (`windowEx.cpp:609`).
    assert_eq!(env.eval_int("w.setMessageHook(1, 512)"), 0);
    assert_eq!(env.eval_int("w.setMessageHook(1)"), 1);

    // Unknown names and out-of-range numbers raise TJS_E_FAIL
    // (`windowEx.cpp:611-612`).
    let error = env.eval_err("w.setMessageHook(1, 'WM_NOT_A_MESSAGE')");
    assert!(error.contains("unknown message"), "unexpected: {error}");
    let error = env.eval_err("w.setMessageHook(1, 1024)");
    assert!(error.contains("out of range"), "unexpected: {error}");
    // 0x3FF is the last accepted number.
    assert_eq!(env.eval_int("w.setMessageHook(1, 1023)"), 1);

    // `!!p[0]->AsInteger()` coercion (`windowEx.cpp:601`,
    // `reference/cpp/core/tjs2/tjsVariant.h:862-878`): 0.5 truncates to 0.
    assert_eq!(env.eval_int("w.setMessageHook(1)"), 1);
    assert_eq!(env.eval_int("w.setMessageHook(0.5)"), 0);
    // A string parses a leading number (`tjsVariantString.cpp:457-466`);
    // "abc" parses as 0 → all-hook false.
    assert_eq!(env.eval_int("w.setMessageHook('1')"), 1);
    assert_eq!(env.eval_int("w.setMessageHook('abc')"), 0);
    // `null` is an object-typed variant; `AsInteger` raises the variant
    // conversion error (`tjsVariant.h:866-867`).
    let error = env.eval_err("w.setMessageHook(null)");
    assert!(!error.is_empty(), "null flag must raise");
}

// ---------------------------------------------------------------------------
// Window: notification table
// ---------------------------------------------------------------------------

/// Reference: `getWindowNotificationNum` reads the `Window._Notifications`
/// dictionary with `getIntValue(key, -1)` (`windowEx.cpp:751-758`) — a
/// case-sensitive member lookup (`reference/cpp/core/tjs2/tjsObject.h:331-336`
/// compares names with `TJS_strcmp`), so a differently-cased name misses and
/// the -1 default comes back. `getWindowNotificationName` reads the reverse
/// direction with `getStrValue(num)` whose default is the empty string
/// (`reference/cpp/core/plugin/ncbind.hpp:703-710`); the reference signature
/// takes exactly one parameter, enforced by ncbind
/// (`reference/cpp/core/plugin/ncbind.hpp:1173-1178`). The parameter
/// converts through `tjs_int`/`AsInteger` (`tjsVariant.h:933-935`), so a
/// string argument parses a leading number.
#[test]
fn notification_lookup_matches_the_reference_table() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    // Exact names only.
    assert_eq!(env.eval_int("w.getNotificationNum('WM_MOUSEMOVE')"), 512);
    assert_eq!(env.eval_int("w.getNotificationNum('wm_mousemove')"), -1);
    assert_eq!(env.eval_int("w.getNotificationNum('WM_NOPE')"), -1);
    // A missing argument is rejected by ncbind.
    assert!(env.eval("w.getNotificationNum()").is_err());
    // Name lookups, including the `AsInteger` string conversion.
    assert_eq!(env.eval_str("w.getNotificationName(512)"), "WM_MOUSEMOVE");
    assert_eq!(
        env.eval_str("w.getNotificationName(160.9)"),
        "WM_NCMOUSEMOVE"
    );
    assert_eq!(
        env.eval_str("w.getNotificationName('160')"),
        "WM_NCMOUSEMOVE"
    );
    // "abc" parses as 0 → WM_NULL; an unknown number reads "".
    assert_eq!(env.eval_str("w.getNotificationName('abc')"), "WM_NULL");
    assert_eq!(env.eval_str("w.getNotificationName(9999)"), "");
    assert!(env.eval("w.getNotificationName()").is_err());
    // The `AsInteger` coercion rejects object arguments.
    assert!(env.eval("w.getNotificationName(%[])").is_err());
}

/// Reference: `_getNotificationVariant` builds `Window._Notifications` as a
/// two-way dictionary — `WM(name) => value` *and* `value => WM(name)` for
/// every entry (`reference/cpp/plugins/windowEx.cpp:796-873`), attached to
/// the `Window` class (`windowEx.cpp:874-876`). Later writes win, so the
/// reverse of the duplicated values resolves to the last name in the macro
/// list: `WM_KEYDOWN` (256), `WM_SETTINGCHANGE` (26), `WM_MOUSELAST` (526),
/// `WM_KEYLAST` (265).
#[test]
fn notification_dictionary_is_installed_on_the_window_class() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    // Both directions, plain entries.
    assert_eq!(env.eval_int("Window._Notifications.WM_CLOSE"), 16);
    assert_eq!(env.eval_str("Window._Notifications[16]"), "WM_CLOSE");
    assert_eq!(env.eval_int("Window._Notifications.WM_MOUSEMOVE"), 512);
    assert_eq!(env.eval_str("Window._Notifications[512]"), "WM_MOUSEMOVE");
    // Duplicated values resolve to the last-written name.
    assert_eq!(env.eval_str("Window._Notifications[256]"), "WM_KEYDOWN");
    assert_eq!(
        env.eval_str("Window._Notifications[26]"),
        "WM_SETTINGCHANGE"
    );
    assert_eq!(env.eval_str("Window._Notifications[526]"), "WM_MOUSELAST");
    assert_eq!(env.eval_str("Window._Notifications[265]"), "WM_KEYLAST");
    // The forward direction of the duplicates stays intact.
    assert_eq!(env.eval_int("Window._Notifications.WM_KEYFIRST"), 256);
    assert_eq!(env.eval_int("Window._Notifications.WM_WININICHANGE"), 26);
    // The table is queryable through a window object too.
    assert_eq!(env.eval_int("w.getNotificationNum('WM_CLOSE')"), 16);
}

// ---------------------------------------------------------------------------
// Window: per-object state
// ---------------------------------------------------------------------------

/// Reference: every `Window` object lazily receives **its own** `WindowEx`
/// native instance (`NCB_GET_INSTANCE_HOOK(WindowEx)` at
/// `reference/cpp/plugins/windowEx.cpp:920-931`) whose flags start out
/// `false` (`windowEx.cpp:711-719`), so flags written on one window are
/// never visible on another.
#[test]
fn window_flags_are_per_window_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var a = new Window(); var b = new Window(); \
         a.disableResize = 1; a.disableMove = 1; a.enableNCMouseEvent = 1;",
    );
    assert_eq!(env.eval_int("a.disableResize"), 1);
    assert_eq!(env.eval_int("a.disableMove"), 1);
    assert_eq!(env.eval_int("a.enableNCMouseEvent"), 1);
    // The second window keeps its own defaults.
    assert_eq!(env.eval_int("b.disableResize"), 0);
    assert_eq!(env.eval_int("b.disableMove"), 0);
    assert_eq!(env.eval_int("b.enableNCMouseEvent"), 0);
}

/// Reference: the native instance lives exactly as long as its script object
/// (it is created on first access and destroyed with the object,
/// `reference/cpp/plugins/windowEx.cpp:920-931`), so a *newly created*
/// window reads the constructor defaults (`windowEx.cpp:711-719`) even when
/// the allocator reuses the address of a released one.
#[test]
fn window_state_is_fresh_for_a_newly_created_window() {
    let _lock = vm_lock();
    let env = Env::new();
    // Several rounds to make address reuse of the freed object likely.
    for _ in 0..8 {
        env.run(
            "var a = new Window(); \
             a.disableResize = 1; a.enableNCMouseEvent = 1; a.exSystemMenu = %[ \"x\" => 1 ]; \
             a = null;",
        );
    }
    env.run("var b = new Window();");
    assert_eq!(env.eval_int("b.disableResize"), 0, "stale disableResize");
    assert_eq!(
        env.eval_int("b.enableNCMouseEvent"),
        0,
        "stale enableNCMouseEvent"
    );
    assert_eq!(
        env.eval_ok("b.exSystemMenu === null"),
        TjsValue::Integer(1),
        "stale exSystemMenu"
    );
}

/// Reference: the flag writes use `!!p[0]->AsInteger()`
/// (`reference/cpp/plugins/windowEx.cpp:507,525,581`) with the coercion of
/// `tTJSVariant::AsInteger` (`reference/cpp/core/tjs2/tjsVariant.h:862-878`):
/// reals truncate toward zero, strings parse a leading number
/// (`reference/cpp/core/tjs2/tjsVariantString.cpp:457-466`), `void` reads 0
/// and object-typed values (including `null`, `tjsVariant.h:552-557`) raise
/// the variant conversion error. Reads return the stored bool as an integer
/// (`windowEx.cpp:496-498`).
#[test]
fn window_flag_writes_coerce_like_variant_as_integer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    // Real truncates toward zero (0.5 → 0 → false).
    assert_eq!(
        env.assign_then("w.disableResize = 0.5;", "w.disableResize"),
        0
    );
    assert_eq!(
        env.assign_then("w.disableResize = -1.9;", "w.disableResize"),
        1
    );
    // Strings parse a leading number.
    assert_eq!(
        env.assign_then("w.disableResize = '2';", "w.disableResize"),
        1
    );
    assert_eq!(
        env.assign_then("w.disableResize = 'abc';", "w.disableResize"),
        0
    );
    assert_eq!(
        env.assign_then("w.disableResize = '-3';", "w.disableResize"),
        1
    );
    // `null` raises the variant conversion error.
    let error = env.eval_err("w.disableResize = null");
    assert!(!error.is_empty(), "null flag must raise");
    // Plain integers round-trip; the other flags behave identically.
    assert_eq!(
        env.assign_then("w.disableResize = 1;", "w.disableResize"),
        1
    );
    assert_eq!(
        env.assign_then("w.disableResize = 0;", "w.disableResize"),
        0
    );
    assert_eq!(env.assign_then("w.disableMove = 1;", "w.disableMove"), 1);
    assert_eq!(
        env.assign_then("w.enableNCMouseEvent = 1;", "w.enableNCMouseEvent"),
        1
    );
}

// ---------------------------------------------------------------------------
// Window: exSystemMenu / setOverlayBitmap
// ---------------------------------------------------------------------------

/// Reference: the getter writes `tTJSVariant(sysMenuModified,
/// sysMenuModified)` (`reference/cpp/plugins/windowEx.cpp:539-545`), which
/// is an object-typed variant — with a null pointer it reads back as `null`
/// (`reference/cpp/core/tjs2/tjsVariant.h:562-571`). The setter stores
/// `p[0]->AsObject()` (`windowEx.cpp:547-560`): objects and `null` are
/// accepted (`tjsVariant.h:682-691` keeps `tvtObject` null), while scalars
/// raise the variant conversion error. `resetExSystemMenu` is a no-op in the
/// vendored reference (`windowEx.cpp:562-565` — and its registration is
/// commented out at `windowEx.cpp:981`), so it must not clear the stored
/// menu.
#[test]
fn ex_system_menu_object_lifecycle() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var menu = %[ \"cmd\" => 7 ]; \
         var fresh = (w.exSystemMenu === null); \
         w.exSystemMenu = menu; \
         var held = w.exSystemMenu.cmd; \
         w.resetExSystemMenu(); \
         var afterReset = w.exSystemMenu.cmd; \
         w.exSystemMenu = null; \
         var cleared = (w.exSystemMenu === null); \
         var menu2 = %[ \"cmd\" => 9 ]; \
         w.exSystemMenu = menu2; \
         var replaced = w.exSystemMenu.cmd;",
    );
    assert_eq!(env.eval_int("fresh"), 1, "unset reads back as null");
    assert_eq!(env.eval_int("held"), 7);
    assert_eq!(
        env.eval_int("afterReset"),
        7,
        "resetExSystemMenu is a no-op in the reference"
    );
    assert_eq!(env.eval_int("cleared"), 1, "assignment of null clears");
    assert_eq!(env.eval_int("replaced"), 9, "reassignment replaces");
    // A scalar write raises the variant conversion error.
    let error = env.eval_err("w.exSystemMenu = 42");
    assert!(!error.is_empty(), "scalar exSystemMenu must raise");
}

/// Reference: `setOverlayBitmap` only enters its body for `n > 0` with a
/// `tvtObject` argument (`reference/cpp/plugins/windowEx.cpp:531-541`).
/// Non-object arguments (integer/string/…) silently return `TJS_S_OK`;
/// `null` *is* `tvtObject` (`reference/cpp/core/tjs2/tjsVariant.h:552-557`)
/// and reaches `OverlayBitmap::setBitmap`, which rejects the null layer and
/// fails (`windowEx.cpp:537-540`); a non-`Layer` object fails the same way
/// (`windowEx.cpp:546-553` of the nested `setBitmap`). Attached at
/// `windowEx.cpp:978`.
#[test]
fn set_overlay_bitmap_validates_layer_objects_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         w.setOverlayBitmap(l);",
    );
    // Missing and non-object arguments are silent successes.
    env.eval_void("w.setOverlayBitmap()");
    env.eval_void("w.setOverlayBitmap(42)");
    env.eval_void("w.setOverlayBitmap('bmp')");
    // `null` and non-Layer objects fail.
    let error = env.eval_err("w.setOverlayBitmap(null)");
    assert!(error.contains("Layer"), "unexpected: {error}");
    let error = env.eval_err("w.setOverlayBitmap(%[])");
    assert!(error.contains("Layer"), "unexpected: {error}");
}

// ---------------------------------------------------------------------------
// Window: registration and callbacks through the object
// ---------------------------------------------------------------------------

/// Reference: `registerExEvent`, `getNotificationNum` and
/// `getNotificationName` are attached as `Method`s on the `Window` class
/// (`reference/cpp/plugins/windowEx.cpp:991-993`); `checkExEvents` probes
/// the script object for `onResizing`/`onMoving`/`onMove`/`onNcMouseMove`
/// (`windowEx.cpp:730-736`) and never fails. The event names come from the
/// `EXEV_*` macros (`windowEx.cpp:316-340`).
///
/// A script-defined member is found before the attached class member
/// (`reference/cpp/core/tjs2/tjsObject.cpp:1214-1225` — `Find` on the
/// object first, `TJS_E_MEMBERNOTFOUND` only then walks the chain), so a
/// script override wins *through the object*.
#[test]
fn register_ex_event_reaches_the_object_and_script_overrides_win() {
    let _lock = vm_lock();
    let env = Env::new();
    // The real-game boot blocker: `Member "registerExEvent" does not exist`.
    env.run("var w = new Window(); w.registerExEvent();");
    // A subclass whose handlers exist is probed without error.
    env.run(
        "class MainWindow extends Window { \
             function onResizing() {} \
             function onNcMouseMove() {} \
         } \
         var mw = new MainWindow(); mw.registerExEvent();",
    );
    // An instance-level script member shadows the native one entirely.
    env.run(
        "class QuietWindow extends Window { \
             function registerExEvent() { return 7; } \
         } \
         var qw = new QuietWindow();",
    );
    assert_eq!(env.eval_int("qw.registerExEvent()"), 7);
    // The native member still answers on plain windows.
    assert_eq!(env.eval_ok("w.registerExEvent()"), TjsValue::Void);
}

/// Reference: the `on*` handler names are the `EXEV_*` macros
/// (`reference/cpp/plugins/windowEx.cpp:316-340`); in this build the native
/// dispatcher (`regist(bool)` at `windowEx.cpp:689-691`) never fires them,
/// and a script-assigned handler is found before anything the class
/// provides (`reference/cpp/core/tjs2/tjsObject.cpp:1214-1225`), so calling
/// one through the object reaches the script function.
#[test]
fn ex_event_handlers_reach_the_script_override() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    for handler in [
        "onMinimize",
        "onMaximize",
        "onMaximizeQuery",
        "onShow",
        "onHide",
        "onResizing",
        "onMoving",
        "onMove",
        "onMoveSizeBegin",
        "onMoveSizeEnd",
        "onDisplayChanged",
        "onEnterMenuLoop",
        "onExitMenuLoop",
        "onActivateChanged",
        "onScreenSave",
        "onMonitorPower",
        "onNcMouseMove",
        "onNcMouseLeave",
        "onNcMouseDown",
        "onNcMouseUp",
        "onExSystemMenuSelected",
        "onStartKeyMenu",
        "onAccelKeyMenu",
        "onNonCapMouseEvent",
        "onWindowsMessageHook",
    ] {
        env.run(&format!("w.{handler}();"));
    }
    // A script override shadows the native no-op *through the object*.
    env.run(
        "var hits = 0; w.onNcMouseMove = function(x, y) { hits = x + y; }; w.onNcMouseMove(2, 3);",
    );
    assert_eq!(env.eval_int("hits"), 5);
}

/// Reference: the `ncht*` class constants are `Variant` registrations with
/// the Win32 `HT*` hit-test codes, including the `& 0xFFFF` wrapping of
/// `HTERROR`/`HTTRANSPARENT` (`reference/cpp/plugins/windowEx.cpp:935-958`).
#[test]
fn window_static_hit_test_constants_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    let expected = [
        ("nchtError", 65534),
        ("nchtTransparent", 65535),
        ("nchtNoWhere", 0),
        ("nchtClient", 1),
        ("nchtCaption", 2),
        ("nchtSysMenu", 3),
        ("nchtSize", 4),
        ("nchtGrowBox", 4),
        ("nchtMenu", 5),
        ("nchtHScroll", 6),
        ("nchtVScroll", 7),
        ("nchtMinButton", 8),
        ("nchtReduce", 8),
        ("nchtMaxButton", 9),
        ("nchtZoom", 9),
        ("nchtLeft", 10),
        ("nchtRight", 11),
        ("nchtTop", 12),
        ("nchtTopLeft", 13),
        ("nchtTopRight", 14),
        ("nchtBottom", 15),
        ("nchtBottomLeft", 16),
        ("nchtBottomRight", 17),
        ("nchtBorder", 18),
    ];
    for (name, value) in expected {
        assert_eq!(
            env.eval_int(&format!("Window.{name}")),
            value,
            "Window.{name}"
        );
    }
}

/// Reference: the full `Window` attachment block — every `RawCallback`,
/// `Method` and property name the plugin adds to `Window`
/// (`reference/cpp/plugins/windowEx.cpp:934-994`), plus the locked surface
/// members `disableMove`/`resetExSystemMenu` whose registrations are
/// commented out in the vendored file but whose bodies exist
/// (`windowEx.cpp:510-529,562-565`).
#[test]
fn every_window_instance_method_is_registered() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         w.minimize(); w.maximize(); w.showRestore(); w.focusMenuByKey(0); \
         w.resetWindowIcon(); w.setWindowIcon(''); \
         w.getWindowRect(); w.getClientRect(); w.getNormalRect(); \
         w.setOverlayBitmap(l); w.resetExSystemMenu(); w.ncHitTest(0, 0); \
         w.setMessageHook(1, 'WM_MOUSEMOVE'); w.setMessageHook(0); \
         w.bringTo(); w.sendToBack(); w.registerExEvent(); \
         w.getNotificationNum('WM_MOUSEMOVE'); w.getNotificationName(512);",
    );
}

// ---------------------------------------------------------------------------
// MenuItemEx
// ---------------------------------------------------------------------------

/// Reference: `popupEx` is a raw attached function with a `TJS_S_OK` body
/// and no argument checks (`reference/cpp/plugins/windowEx.cpp:1185-1188`,
/// attached at `windowEx.cpp:1239`), so any argument count is accepted and
/// the call reads back as void.
#[test]
fn popup_ex_accepts_any_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var m = new MenuItem(null, 'Ex');");
    env.eval_void("m.popupEx()");
    env.eval_void("m.popupEx(0)");
    env.eval_void("m.popupEx(0, 1, 2, 3)");
}

/// Reference: `rightJustify` reads `rj > 0` with `rj` starting at `-1`
/// (`reference/cpp/plugins/windowEx.cpp:1082,1145`), so the initial read is
/// 0; the write is `rj = !!v.AsInteger()`
/// (`windowEx.cpp:1084-1088`) with the `AsInteger` coercion of
/// `reference/cpp/core/tjs2/tjsVariant.h:862-878` (reals truncate, strings
/// parse a leading number, objects raise). Attached at `windowEx.cpp:1231`.
#[test]
fn right_justify_round_trips_with_variant_coercion() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var m = new MenuItem(null, 'Ex');");
    assert_eq!(env.eval_int("m.rightJustify"), 0, "initial rj = -1");
    assert_eq!(env.assign_then("m.rightJustify = 1;", "m.rightJustify"), 1);
    assert_eq!(env.assign_then("m.rightJustify = 0;", "m.rightJustify"), 0);
    // `!!AsInteger()`: 0.9 truncates to 0, "2" parses to 2, "abc" to 0.
    assert_eq!(
        env.assign_then("m.rightJustify = 0.9;", "m.rightJustify"),
        0
    );
    assert_eq!(
        env.assign_then("m.rightJustify = '2';", "m.rightJustify"),
        1
    );
    assert_eq!(
        env.assign_then("m.rightJustify = 'abc';", "m.rightJustify"),
        0
    );
    // `null` raises the variant conversion error.
    let error = env.eval_err("m.rightJustify = null");
    assert!(!error.is_empty(), "null rightJustify must raise");
}

/// Reference: `setBmpSelect` switches on the variant type — object-typed
/// values (including `null`, `tjsVariant.h:552-557`) store `BMT_BMP` and
/// read back as `-1`; void/integer/string store `BMT_SYS` and reals fall
/// through, all reading back as `0` (`reference/cpp/plugins/windowEx.cpp:1104-1127`).
/// The three properties are independent (`windowEx.cpp:1090-1102`).
#[test]
fn bitmap_properties_track_the_variant_type() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var m = new MenuItem(null, 'Ex'); \
         var bmp = %[ \"a\" => 1 ];",
    );
    // An object stores BMT_BMP → -1.
    assert_eq!(env.assign_then("m.bmpItem = bmp;", "m.bmpItem"), -1);
    // An integer stores BMT_SYS → 0.
    assert_eq!(env.assign_then("m.bmpItem = 3;", "m.bmpItem"), 0);
    // A string stores BMT_SYS → 0 (even a numeric one).
    assert_eq!(env.assign_then("m.bmpItem = '7';", "m.bmpItem"), 0);
    // A real has no switch case → BMT_NONE → 0.
    assert_eq!(env.assign_then("m.bmpItem = 1.5;", "m.bmpItem"), 0);
    // `null` is tvtObject → BMT_BMP → -1.
    assert_eq!(env.assign_then("m.bmpItem = null;", "m.bmpItem"), -1);
    // The three properties are independent.
    env.run("m.bmpChecked = bmp; m.bmpUnchecked = 3;");
    assert_eq!(env.eval_int("m.bmpItem"), -1);
    assert_eq!(env.eval_int("m.bmpChecked"), -1);
    assert_eq!(env.eval_int("m.bmpUnchecked"), 0);
}

/// Reference: like `Window`, every `MenuItem` object lazily receives **its
/// own** `MenuItemEx` native instance (`NCB_GET_INSTANCE_HOOK(MenuItemEx)`
/// at `reference/cpp/plugins/windowEx.cpp:1191-1199`) whose constructor
/// initialises `rj = -1` (`windowEx.cpp:1158`); the getter reads
/// `rj > 0` (`windowEx.cpp:1082`), so a newly created item reads 0 even when
/// the allocator reuses a released item's address.
#[test]
fn menu_item_state_is_fresh_for_a_newly_created_item() {
    let _lock = vm_lock();
    let env = Env::new();
    // Several rounds to make address reuse of the freed object likely.
    for _ in 0..8 {
        env.run(
            "var m = new MenuItem(null, 'Ex'); \
             m.rightJustify = 1; \
             m = null;",
        );
    }
    env.run("var n = new MenuItem(null, 'Ex');");
    assert_eq!(env.eval_int("n.rightJustify"), 0, "stale rightJustify");
}

/// Reference: the `bi*` bitmap constants are registered as `tjs_int64`
/// `Variant`s with the `HBMMENU_*` values
/// (`reference/cpp/plugins/windowEx.cpp:1220-1229`).
#[test]
fn menu_item_static_bitmap_constants_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    for (name, value) in [
        ("biSystem", 1),
        ("biRestore", 2),
        ("biMinimize", 3),
        ("biClose", 5),
        ("biCloseDisabled", 6),
        ("biMinimizeDisabled", 7),
        ("biPopupClose", 8),
        ("biPopupRestore", 9),
        ("biPopupMaximize", 10),
        ("biPopupMinimize", 11),
    ] {
        assert_eq!(
            env.eval_int(&format!("MenuItem.{name}")),
            value,
            "MenuItem.{name}"
        );
    }
}

// ---------------------------------------------------------------------------
// ScriptsEx
// ---------------------------------------------------------------------------

/// Reference: `outputErrorLogOnEval` defaults to `true`
/// (`reference/cpp/plugins/windowEx.cpp:1577`); `setEvalErrorLog(flag)`
/// stores the flag and returns the previous one (`windowEx.cpp:1535-1539`,
/// a `bool` result written as an integer, `reference/cpp/core/tjs2/tjsVariant.cpp:630-635`)
/// and is attached as a one-parameter function, which ncbind enforces
/// (`reference/cpp/core/plugin/ncbind.hpp:1173-1178`). `eval` requires at
/// least one argument in *both* flag states: the default path delegates to
/// the original `Scripts.eval` (`windowEx.cpp:1544-1545`, whose own check is
/// `reference/cpp/core/base/ScriptMgnIntf.cpp:1412-1414`) and the override
/// path checks `n < 1` itself (`windowEx.cpp:1547-1548`). Evaluation errors
/// raise in both states — the flag only controls logging
/// (`windowEx.cpp:1544-1560`).
#[test]
fn scripts_eval_and_flag_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    // The default is true, so the first call returns 1.
    assert_eq!(env.eval_int("Scripts.setEvalErrorLog(0)"), 1);
    assert_eq!(env.eval_int("Scripts.eval('20 + 22')"), 42);
    // A bad expression still raises (the flag only controls logging).
    let error = env.eval_err("Scripts.eval('this is not valid ;;;')");
    assert!(!error.is_empty(), "expected a typed eval error");
    // Missing argument raises in the flag=false state…
    assert!(env.eval("Scripts.eval()").is_err());
    // …and with the default flag=true state (delegated to the original eval).
    assert_eq!(env.eval_int("Scripts.setEvalErrorLog(1)"), 0);
    assert!(env.eval("Scripts.eval()").is_err());
    assert_eq!(env.eval_int("Scripts.eval('6 * 7')"), 42);
    // ncbind enforces the flag parameter.
    assert!(env.eval("Scripts.setEvalErrorLog()").is_err());
    // A numeric string parses as a number (`tjsVariantString.cpp:457-466`).
    assert_eq!(env.eval_int("Scripts.setEvalErrorLog('2')"), 1);
    assert_eq!(env.eval_int("Scripts.eval('1 + 1')"), 2);
}

// ---------------------------------------------------------------------------
// ConsoleEx / PadEx
// ---------------------------------------------------------------------------

/// Reference: `ConsoleEx` bodies are `return TJS_S_OK;` stubs — `getRect`
/// and `getPlacement` never write a result (so they read back as **void**),
/// `restoreMaximize`/`maximize`/`bringAfter` are inert, `setPos` only
/// checks `n < 2`, and `setPlacement` checks `n < 1` and the `tvtObject`
/// type (`reference/cpp/plugins/windowEx.cpp:1290-1331`). The members are
/// attached to the `Debug.console` singleton (`windowEx.cpp:1333-1346`);
/// `null` passes the object-type check (`reference/cpp/core/tjs2/tjsVariant.h:552-557`).
#[test]
fn console_members_validate_arguments_and_return_void() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var c = Debug.console;");
    // Inert bodies return void.
    env.eval_void("c.restoreMaximize()");
    env.eval_void("c.maximize()");
    env.eval_void("c.bringAfter()");
    // The stubs never fill the result slot.
    env.eval_void("c.getRect()");
    env.eval_void("c.getPlacement()");
    // setPos: argument-count check only.
    env.eval_void("c.setPos(3, 4)");
    assert!(env.eval("c.setPos(3)").is_err(), "setPos needs 2 arguments");
    // setPlacement: argument count and object type.
    env.eval_void("c.setPlacement(%[ \"a\" => 1 ])");
    env.eval_void("c.setPlacement(null)");
    assert!(env.eval("c.setPlacement()").is_err(), "needs 1 argument");
    let error = env.eval_err("c.setPlacement(42)");
    assert!(error.contains("object"), "unexpected: {error}");
}

/// Reference: `PadEx::registerExEvents` is an empty body attached with
/// `Method` (`reference/cpp/plugins/windowEx.cpp:1396,1416-1418`), so the
/// call is accepted, takes any arguments and returns void.
#[test]
fn pad_register_ex_event_is_callable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var p = new Pad(); p.registerExEvent();");
    assert_eq!(env.eval_ok("p.registerExEvent()"), TjsValue::Void);
    assert_eq!(env.eval_ok("p.registerExEvent(1, 2)"), TjsValue::Void);
}

// ---------------------------------------------------------------------------
// System: argument validation
// ---------------------------------------------------------------------------

/// Reference: the `System` attachment block
/// (`reference/cpp/plugins/windowEx.cpp:1513-1527`) — the vendored bodies
/// validate exactly like this: `setCursorPos` requires two arguments
/// (`windowEx.cpp:1441-1446`), `getSystemMetrics` requires one *string*
/// argument and rejects the empty string (`windowEx.cpp:1448-1457`),
/// `readEnvValue` does the same (`windowEx.cpp:1485-1494`),
/// `expandEnvString` requires one argument (`windowEx.cpp:1497-1501`),
/// `setApplicationIcon` validates nothing (`windowEx.cpp:1504-1507`), and
/// `setIconicPreview(bool en)` is a one-parameter ncbind function
/// (`windowEx.cpp:1509`, `reference/cpp/core/plugin/ncbind.hpp:1173-1178`).
#[test]
fn system_argument_validation_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    // setCursorPos: two arguments, then accepted.
    assert!(env.eval("System.setCursorPos(1)").is_err());
    env.eval_void("System.setCursorPos(1, 2)");
    // getSystemMetrics: one string argument, non-empty.
    assert!(env.eval("System.getSystemMetrics()").is_err());
    let error = env.eval_err("System.getSystemMetrics(42)");
    assert!(error.contains("string"), "unexpected: {error}");
    let error = env.eval_err("System.getSystemMetrics('')");
    assert!(!error.is_empty(), "empty metric name must raise");
    // readEnvValue: same validation.
    assert!(env.eval("System.readEnvValue()").is_err());
    let error = env.eval_err("System.readEnvValue(42)");
    assert!(error.contains("string"), "unexpected: {error}");
    let error = env.eval_err("System.readEnvValue('')");
    assert!(!error.is_empty(), "empty variable name must raise");
    // expandEnvString: one argument.
    assert!(env.eval("System.expandEnvString()").is_err());
    // setApplicationIcon: no argument check at all.
    env.eval_void("System.setApplicationIcon()");
    env.eval_void("System.setApplicationIcon('icon.ico')");
    // setIconicPreview: one argument, returns true regardless of the flag.
    assert!(env.eval("System.setIconicPreview()").is_err());
    assert_eq!(env.eval_int("System.setIconicPreview(1)"), 1);
    assert_eq!(env.eval_int("System.setIconicPreview(0)"), 1);
}

/// Reference: `getSystemMetrics` ensures the `System.metrics` dictionary
/// exists on **every** call (`TJS_MEMBERENSURE` write,
/// `reference/cpp/plugins/windowEx.cpp:1466-1476`), uppercases the lookup
/// key (`windowEx.cpp:1458`) and reads it with `getIntValue(key, -1)`
/// (`windowEx.cpp:1478-1481`, default at
/// `reference/cpp/core/plugin/ncbind.hpp:689-695`); a negative reading is
/// `TJS_E_INVALIDPARAM`. The port keeps that path *before* consulting the
/// host seam, so a script can populate `System.metrics` exactly like the
/// reference; without a dictionary value and without an installed host
/// provider the port raises its typed D11 error instead of returning void.
#[test]
fn get_system_metrics_creates_and_reads_the_metrics_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();
    // First call: creates `System.metrics`, finds nothing, no host → error.
    let error = env.eval_err("System.getSystemMetrics('SM_CXSCREEN')");
    assert!(
        error.contains("D11") || error.contains("provider"),
        "unexpected: {error}"
    );
    // The dictionary creation is observable (`windowEx.cpp:1470-1476`).
    assert_eq!(env.eval_str("typeof System.metrics"), "Object");
    // A script-populated value is found — through the uppercase key.
    env.run("System.metrics.SM_CXSCREEN = 1024;");
    assert_eq!(env.eval_int("System.getSystemMetrics('sm_cxscreen')"), 1024);
    // The reference rejects negative readings (`windowEx.cpp:1480-1481`).
    env.run("System.metrics.SM_BAD = -1;");
    let error = env.eval_err("System.getSystemMetrics('SM_BAD')");
    assert!(
        error.contains("D11") || error.contains("provider"),
        "unexpected: {error}"
    );
    // A missing key still raises.
    assert!(env.eval("System.getSystemMetrics('SM_NOPE')").is_err());
}

// ---------------------------------------------------------------------------
// System: literals and host facts
// ---------------------------------------------------------------------------

/// Reference literals — these members never query a host in the reference:
/// `getDoubleClickTime` is `static tjs_int getDoubleClickTime() { return
/// -1; }` (`reference/cpp/plugins/windowEx.cpp:1422`), `setIconicPreview`
/// returns `true` regardless of its flag (`windowEx.cpp:1509`), `breathe` /
/// `isBreathing` are the core `TVPBreathe` (void) and `TVPGetBreathing`
/// (bool → integer, `reference/cpp/core/tjs2/tjsVariant.cpp:630-635`)
/// attached at `windowEx.cpp:1523-1524`, `getAboutString` returns a string
/// (`reference/cpp/core/base/MsgIntf.cpp:144`, attached at
/// `windowEx.cpp:1526`), and `getCPUType` is the **`tjs_uint32`
/// CPU-feature bitmask** `TVPGetCPUType` (`windowEx.cpp:1527`,
/// `reference/cpp/core/environ/DetectCPU.cpp:170-175`) — i.e. an
/// *integer*, not a string. The port reads 0 for the bitmask until the host
/// installs the detected flags (L3 owns no platform code).
#[test]
fn system_literals_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(env.eval_int("System.getDoubleClickTime()"), -1);
    assert_eq!(env.eval_int("System.setIconicPreview(0)"), 1);
    assert_eq!(env.eval_ok("System.breathe()"), TjsValue::Void);
    assert_eq!(env.eval_int("System.isBreathing()"), 0);
    assert!(!env.eval_str("System.getAboutString()").is_empty());
    assert_eq!(env.eval_int("System.getCPUType()"), 0);
    // The CPU type is an integer — a string result would break `&` masks.
    assert!(matches!(
        env.eval_ok("System.getCPUType()"),
        TjsValue::Integer(_)
    ));
}

/// Reference: `getDisplayMonitors`, `getMonitorInfo` and `getCursorPos` are
/// Win32 bodies the vendored file stubs to bare `return TJS_S_OK;` without a
/// result (`reference/cpp/plugins/windowEx.cpp:1424-1446`) — the port must
/// not fabricate values for them either: until the host installs a provider
/// they raise the typed D11 error (ground rule: typed errors, no stubs).
/// `readEnvValue`/`expandEnvString` pass their reference validation first
/// (`windowEx.cpp:1485-1501`) and are host facts from there on;
/// `getSystemMetrics` falls through to D11 only after its reference
/// `System.metrics` lookup misses (`windowEx.cpp:1466-1481`).
#[test]
fn system_host_facts_raise_d11_until_installed() {
    let _lock = vm_lock();
    let env = Env::new();
    for expression in [
        "System.getDisplayMonitors()",
        "System.getMonitorInfo(0)",
        "System.getCursorPos()",
        "System.getSystemMetrics('SM_CXSCREEN')",
        "System.readEnvValue('PATH')",
        "System.expandEnvString('%PATH%')",
    ] {
        let error = env.eval_err(expression);
        assert!(
            error.contains("D11") || error.contains("provider"),
            "{expression} did not raise the host-fact error: {error}"
        );
    }
    // The reference literals above answer without a host (see the
    // `windowEx.cpp:1422,1509,1523-1527` citations in
    // `system_literals_match_the_reference`).
    assert_eq!(env.eval_int("System.getDoubleClickTime()"), -1);
    assert_eq!(env.eval_int("System.setIconicPreview(1)"), 1);
    assert_eq!(env.eval_ok("System.breathe()"), TjsValue::Void);
    assert_eq!(env.eval_int("System.isBreathing()"), 0);
}

/// Reference: with a provider installed the members return the host's
/// values — the shapes come from the reference code paths the seam
/// replaces: an array of monitor rectangles for `getDisplayMonitors`
/// (`reference/cpp/plugins/windowEx.cpp:1424-1428`), an integer metric
/// (`windowEx.cpp:1478-1481`, uppercased at `windowEx.cpp:1458`), a string
/// environment value (`windowEx.cpp:1485-1494`), and the `tjs_uint32`
/// CPU-feature integer (`windowEx.cpp:1527`).
#[test]
fn system_host_facts_work_once_installed() {
    let _lock = vm_lock();
    let env = Env::new();
    env.host().set_window_ex_display_monitors(|| {
        Ok(vec![
            (0, 0, 1920, 1080, 0, 0, 1920, 1040, true),
            (1920, 0, 3840, 1080, 1920, 0, 3840, 1040, false),
        ])
    });
    env.host().set_window_ex_monitor_info(|index| {
        if index == 0 {
            Ok((0, 0, 1920, 1080, 0, 0, 1920, 1040, true))
        } else {
            Err("no such monitor".into())
        }
    });
    env.host().set_window_ex_cursor_pos(|| Ok((123, 456)));
    env.host().set_window_ex_set_cursor_pos(|x, y| {
        assert_eq!((x, y), (7, 9));
        Ok(())
    });
    env.host().set_window_ex_system_metrics(|name| match name {
        "SM_CXSCREEN" => Ok(1920),
        other => Err(format!("unknown metric {other}")),
    });
    env.host()
        .set_window_ex_read_env(|name| Ok((name == "KRKR_TEST").then(|| "yes".to_string())));
    env.host()
        .set_window_ex_expand_env(|value| Ok(value.replace("%NAME%", "krkr")));
    env.host().set_window_ex_double_click_time(|| Ok(500));
    env.host().set_window_ex_cpu_type(|| Ok(0x2));

    assert_eq!(env.eval_int("System.getDisplayMonitors().count"), 2);
    assert_eq!(env.eval_int("System.getDisplayMonitors()[0].left"), 0);
    assert_eq!(env.eval_int("System.getDisplayMonitors()[1].right"), 3840);
    assert_eq!(env.eval_int("System.getDisplayMonitors()[0].primary"), 1);
    assert_eq!(env.eval_int("System.getMonitorInfo(0).workBottom"), 1040);
    assert_eq!(env.eval_int("System.getCursorPos().x"), 123);
    assert_eq!(env.eval_int("System.getCursorPos().y"), 456);
    env.eval_void("System.setCursorPos(7, 9)");
    // The lookup key is uppercased before it reaches the host
    // (`windowEx.cpp:1458`).
    assert_eq!(env.eval_int("System.getSystemMetrics('sm_cxscreen')"), 1920);
    assert_eq!(env.eval_str("System.readEnvValue('KRKR_TEST')"), "yes");
    assert_eq!(
        env.eval_ok("System.readEnvValue('KRKR_UNSET')"),
        TjsValue::Void
    );
    assert_eq!(
        env.eval_str("System.expandEnvString('%NAME% (x)')"),
        "krkr (x)"
    );
    assert_eq!(env.eval_int("System.getDoubleClickTime()"), 500);
    assert_eq!(env.eval_int("System.getCPUType()"), 2);

    // A provider error becomes a typed TJS error.
    let error = env.eval_err("System.getSystemMetrics('SM_CYCAPTION')");
    assert!(error.contains("unknown metric"), "unexpected: {error}");
}

//! Reference-surface pinning tests.
//!
//! Each `#[test]` pins one divergence found by the member-by-member audit
//! against `reference/cpp` — the behaviour the reference implementation
//! demonstrates at the cited `file:line`. Fixtures that belong to a class
//! suite with an editable test file (`kag`, `cdda_midi`, `scripts`,
//! `storages`, `storages_io`) live in that suite instead; everything else
//! that had no editable home is here.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed so its address stays stable after
/// [`CompatHost::install`] stored it, and the host is declared before the
/// engine so its `Drop` removes the binding while the VM is still alive.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// The attached L2 scene when the test constructs visual objects.
    _scene: Option<Box<engine::Scene>>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: None,
        }
    }

    /// Like [`Self::new`], with an attached L2 scene so `new Window()` works.
    fn with_scene() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(engine::Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the binding (it is declared after
        // the host and dropped later).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut engine::Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: Some(scene),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "reference-surface")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "reference-surface")
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

    /// Run one frame-body `async_trigger_poll` step.
    fn poll(&self) {
        let mut ctx = self._host.ctx().expect("bound host");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }
}

// ---------------------------------------------------------------------------
// AsyncTrigger (base/EventIntf.cpp)
// ---------------------------------------------------------------------------

#[test]
fn async_trigger_action_receives_the_reference_event_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();
    // `TVP_ACTION_INVOKE_BEGIN(0, "onFire", objthis)` +
    // `TVP_ACTION_INVOKE_END_NAME` builds `{type, target: objthis}` and
    // passes it to the action (EventIntf.h:208-237, EventIntf.cpp:1132-1147).
    env.run(
        "var seen = ''; var t = new AsyncTrigger(function(ev) { \
         seen = ev.type + ',' + (ev.target === t ? 1 : 0); }, ''); \
         t.onFire();",
    );
    assert_eq!(env.eval_str("seen"), "onFire,1");
}

#[test]
fn async_trigger_cached_retrigger_moves_the_fire_to_the_end() {
    let _lock = vm_lock();
    let env = Env::new();
    // A cached `Trigger()` cancels the previously posted event and re-posts
    // (EventIntf.cpp:1043-1060), so re-triggering A after B fires B first.
    env.run(
        "var log = ''; \
         var a = new AsyncTrigger(function() { log += 'a'; }, ''); \
         var b = new AsyncTrigger(function() { log += 'b'; }, ''); \
         a.trigger(); b.trigger(); a.trigger();",
    );
    env.poll();
    assert_eq!(env.eval_str("log"), "ba");
}

// ---------------------------------------------------------------------------
// Rect (visual/RectItf.cpp)
// ---------------------------------------------------------------------------

#[test]
fn rect_binary_methods_raise_on_a_non_object_argument() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var a = new Rect(1, 2, 3, 4);");
    // `param[0]->AsObjectClosureNoAddRef()` throws for a non-object
    // (tjsVariant.h:728-736; RectItf.cpp:125-127).
    assert!(env.eval("a.clip(5)").is_err(), "clip(5) must raise");
    assert!(
        env.eval("a.intersects('x')").is_err(),
        "intersects('x') must raise"
    );
    // `null` reaches `if(clo.Object)` and skips silently
    // (RectItf.cpp:129-131).
    assert_eq!(env.eval_ok("a.clip(null)"), TjsValue::Void);
    // The receiver is unchanged by the skipped call.
    assert_eq!(env.eval_int("a.left"), 1);
}

#[test]
fn rect_copy_constructor_rejects_non_rect_objects() {
    let _lock = vm_lock();
    let env = Env::new();
    // A plain dictionary is the reference's failed `NativeInstanceSupport`
    // → `TJS_E_INVALIDPARAM` (RectItf.cpp:12-16); a non-object is the
    // conversion throw.
    assert!(
        env.eval("new Rect(%[])").is_err(),
        "a dictionary is not a Rect"
    );
    assert!(env.eval("new Rect(5)").is_err(), "a number is not a Rect");
    assert_eq!(env.eval_int("(new Rect(1, 2, 3, 4)).right"), 3);
}

#[test]
fn rect_numeric_arguments_use_the_reference_integer_conversion() {
    let _lock = vm_lock();
    let env = Env::new();
    // `(tjs_int)` conversions truncate reals and raise on objects
    // (RectItf.cpp:93-98, tjsVariant.h:933-935).
    env.run("var a = new Rect(); a.setSize(2.9, 3.9);");
    assert_eq!(env.eval_int("a.right"), 2);
    assert!(
        env.eval("a.setSize(1, %[])").is_err(),
        "an object is not a size"
    );
    assert!(
        env.eval("a.set(%[], 1, 2, 3)").is_err(),
        "an object is not an edge"
    );
}

// ---------------------------------------------------------------------------
// MenuItem (visual/MenuItemIntf.cpp)
// ---------------------------------------------------------------------------

#[test]
fn menu_item_invalidate_cascades_and_releases_retained_objects() {
    let _lock = vm_lock();
    let env = Env::new();
    // Reference `Invalidate` (MenuItemIntf.cpp:75-107) cascades into every
    // child (`item->Owner->Invalidate(...)`), clears the child list and drops
    // `Window`/`Parent`. After `invalidate`, the VM refuses direct dispatch on
    // the invalidated objects ("native instance method called on an
    // invalidated object", tjs2_abi.cpp:1116-1119).
    env.run(
        "var root = new MenuItem(null, 'R'); \
         var child = new MenuItem(null, 'C'); \
         root.add(child); \
         var fin = ''; \
         root.finalize = function() { fin += 'R'; }; \
         child.finalize = function() { fin += 'C'; }; \
         invalidate root;",
    );
    // The VM runs each script `finalize` before the native `Invalidate`
    // (tjsObject.cpp:389-405); the parent's native hook cascades into the
    // child, so both finalizers run — parent first.
    assert_eq!(env.eval_str("fin"), "RC");
    // After `invalidate`, direct dispatch on the invalidated objects raises
    // — the subtree can neither fire nor answer member reads.
    assert!(
        env.eval("child.caption").is_err(),
        "the child is invalidated"
    );
    assert!(
        env.eval("root.fireClick()").is_err(),
        "the root is invalidated"
    );
}

#[test]
fn menu_item_fire_click_is_dropped_while_events_are_disabled() {
    let _lock = vm_lock();
    let env = Env::with_scene();
    // `TVPPostEvent(..., TVP_EPT_IMMEDIATE, ...)` drops the event while
    // `System.eventDisabled` is set (EventIntf.cpp:210-224), but the
    // reference `OnClick` still runs `TVPDoSaveSystemVariables` afterwards
    // (MenuItemIntf.cpp:215-216) — which is a no-op without a `kag` global.
    env.run(
        "var n = 0; \
         var w = new Window(); \
         var owner = %[ action: function() { n = n + 1; } ]; \
         var m = new MenuItem(owner, w); \
         System.eventDisabled = true; \
         m.fireClick(); \
         System.eventDisabled = false; \
         m.fireClick();",
    );
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn menu_item_action_dispatch_has_no_fallback_members() {
    let _lock = vm_lock();
    let env = Env::with_scene();
    // The default handler invokes **only** `owner.action(event)`; the result
    // of the unchecked `FuncCall` is ignored (MenuItemIntf.cpp:305-316,
    // EventIntf.h:225-237). An owner without `action` is a silent no-op —
    // there is no `owner.onClick()` or direct-call fallback.
    env.run(
        "var seen = 0; var w = new Window(); \
         var noAction = %[ onClick: function() { seen += 100; } ]; \
         var m = new MenuItem(noAction, w); \
         m.fireClick(); \
         var owner = %[ action: function(ev) { seen = seen + (ev.type === 'onClick' ? 1 : 0); } ]; \
         var m2 = new MenuItem(owner, w); \
         m2.fireClick();",
    );
    assert_eq!(env.eval_int("seen"), 1, "only owner.action(event) runs");
}

#[test]
fn menu_item_constructor_validation_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    // An omitted first argument is tolerated (MenuItemImpl.cpp:106-107
    // ignores the base's BADPARAMCOUNT); `null` is a valid action owner; a
    // non-object throws (tjsVariant.h:719-726); a non-Window second object
    // argument raises `TVPSpecifyWindow` (MenuItemIntf.cpp:61-70).
    env.run("var a = new MenuItem(); var b = new MenuItem(null, 'Cap');");
    assert_eq!(env.eval_str("b.caption"), "Cap");
    assert!(
        env.eval("new MenuItem(5)").is_err(),
        "a number is not an owner"
    );
    assert!(
        env.eval("new MenuItem(null, %[])").is_err(),
        "a plain object is not a Window"
    );
}

#[test]
fn menu_item_add_keeps_the_position_of_an_existing_child() {
    let _lock = vm_lock();
    let env = Env::new();
    // `ObjectVector::Add` rejects duplicates (MenuItemIntf.h:32-39), so
    // re-adding an existing child is a no-op that does not move it.
    env.run(
        "var root = new MenuItem(); var a = new MenuItem(null, 'A'); \
         var b = new MenuItem(null, 'B'); \
         root.add(a); root.add(b); root.add(a);",
    );
    assert_eq!(env.eval_int("root.children[0] === a"), 1);
    assert_eq!(env.eval_int("root.children[1] === b"), 1);
}

#[test]
fn menu_item_shortcut_tables_only_contain_the_reference_entries() {
    let _lock = vm_lock();
    let env = Env::new();
    // `CreateShortCutKeyCodeTable` registers exactly three entries; the
    // `MapVirtualKey` loop is `#if 0`-disabled (MenuItemImpl.cpp:363-395).
    assert_eq!(env.eval_int("MenuItem.textToKeycode['bksp']"), 8);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['pgup']"), 33);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['pgdn']"), 34);
    assert_eq!(env.eval_str("MenuItem.keycodeToText[8]"), "BkSp");
    // A key the reference never registers reads as void (`+ 0` → 0).
    assert_eq!(env.eval_int("MenuItem.textToKeycode['tab'] + 0"), 0);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['a'] + 0"), 0);
}

// ---------------------------------------------------------------------------
// System (base/SystemIntf.cpp)
// ---------------------------------------------------------------------------

#[test]
fn system_class_has_the_reference_default_members() {
    let _lock = vm_lock();
    let env = Env::new();
    // The `System` class object carries `exceptionHandler`, `onActivate` and
    // `onDeactivate` default data members, all `null`
    // (SystemIntf.cpp:477-484).
    assert_eq!(env.eval_int("System.onActivate === null ? 1 : 0"), 1);
    assert_eq!(env.eval_int("System.onDeactivate === null ? 1 : 0"), 1);
    assert_eq!(env.eval_int("System.exceptionHandler === null ? 1 : 0"), 1);
    // Assigning replaces the null default like any data member.
    env.run("System.onActivate = function() { return 1; };");
    assert_eq!(
        env.eval_int("(function(){ return System.onActivate() ; })()"),
        1
    );
}

#[test]
fn system_get_argument_has_no_default_parameter() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("System.setArgument('-k', 'v');");
    assert_eq!(env.eval_str("System.getArgument('-k')"), "v");
    // The reference clears the result on a miss — there is no default
    // parameter (SystemImpl.cpp:709-726).
    assert_eq!(
        env.eval_ok("System.getArgument('-missing')"),
        TjsValue::Void
    );
    // A second argument is ignored, not a default.
    assert_eq!(
        env.eval_ok("System.getArgument('-missing', 7)"),
        TjsValue::Void
    );
}

#[test]
fn system_create_uuid_ignores_extra_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // The reference `createUUID` body has no `numparams` check
    // (SystemIntf.cpp:259-286).
    let uuid = env.eval_str("System.createUUID(1)");
    assert_eq!(uuid.len(), 36, "RFC-4122 shape: {uuid}");
}

#[test]
fn system_touch_images_validates_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    // A non-object first argument is the `AsObjectClosureNoAddRef` throw
    // (SystemIntf.cpp:224-231); the optional `limit`/`timeout` convert with
    // `(tjs_int64)` and raise on objects (SystemIntf.cpp:237-244). The
    // pre-caching itself has no L2 counterpart.
    assert_eq!(env.eval_ok("System.touchImages([])"), TjsValue::Void);
    assert!(
        env.eval("System.touchImages(5)").is_err(),
        "a number is not an array"
    );
    assert!(
        env.eval("System.touchImages()").is_err(),
        "arity is checked"
    );
    assert!(
        env.eval("System.touchImages([], {})").is_err(),
        "an object is not a limit"
    );
}

#[test]
fn system_strict_integer_writers_reject_objects() {
    let _lock = vm_lock();
    let env = Env::new();
    // `(tjs_int)` conversions (SystemIntf.cpp:413-416, 366-369, 399-402;
    // SystemImpl.cpp:639-646) raise on objects, while `eventDisabled` uses
    // `param->operator bool()` (SystemIntf.cpp:347-351) which accepts them.
    assert!(env.eval("System.drawThreadNum = %[]").is_err());
    assert!(env.eval("System.graphicCacheLimit = %[]").is_err());
    assert!(env.eval("System.exitOnWindowClose = %[]").is_err());
    assert!(env.eval("System.getKeyState({}, false)").is_err());
    env.run("System.eventDisabled = %[];");
    assert_eq!(env.eval_int("System.eventDisabled"), 1);
    env.run("System.eventDisabled = false;");
}

// ---------------------------------------------------------------------------
// Debug (utils/DebugIntf.cpp)
// ---------------------------------------------------------------------------

#[test]
fn debug_get_last_log_uses_the_reference_window_and_timestamps() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("Debug.message('one'); Debug.notice('two');");
    // The default window is `TVPLogMaxLines + 100` lines — every entry is
    // returned, each formatted `Time + ' ' + Log + "\r\n"` with the
    // `%H:%M:%S` localtime stamp (`DebugIntf.cpp:1454-1457`, `419-459`).
    let text = env.eval_str("Debug.getLastLog()");
    let lines: Vec<&str> = text.split("\r\n").collect();
    assert_eq!(lines.len(), 3, "two entries + trailing separator: {text:?}");
    assert!(ends_with_log_line(lines[0], "one"), "{:?}", lines[0]);
    assert!(ends_with_log_line(lines[1], "two"), "{:?}", lines[1]);
    assert_eq!(lines[2], "");
    // `n == 0` yields the empty string (DebugIntf.cpp:430-431).
    assert_eq!(env.eval_str("Debug.getLastLog(0)"), "");
    // The argument converts with `AsInteger` — objects raise
    // (DebugIntf.cpp:1455-1456).
    assert!(env.eval("Debug.getLastLog({})").is_err());
}

/// `line` is `HH:MM:SS <text>`.
fn ends_with_log_line(line: &str, text: &str) -> bool {
    let bytes = line.as_bytes();
    line.len() > text.len()
        && line.ends_with(text)
        && bytes.len() >= 8
        && bytes[2] == b':'
        && bytes[5] == b':'
        && bytes[..2].iter().all(u8::is_ascii_digit)
        && bytes[3..5].iter().all(u8::is_ascii_digit)
        && bytes[6..8].iter().all(u8::is_ascii_digit)
}

#[test]
fn debug_logging_handlers_receive_the_prefixed_line_and_validate_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // The handler is invoked through `TVPOnLog(buf)` where `buf` is
    // `" " + line` (DebugIntf.cpp:377-391, 122-147).
    env.run(
        "var got = []; \
         Debug.addLoggingHandler(function(s) { got.push(s); }); \
         Debug.message('one');",
    );
    assert_eq!(env.eval_str("got[0]"), " one");
    // `AsObjectClosureNoAddRef` raises for a non-object; TJS `null` is an
    // empty closure that is stored but never invoked (DebugIntf.cpp:645-648).
    assert!(env.eval("Debug.addLoggingHandler(42)").is_err());
    assert!(env.eval("Debug.removeLoggingHandler(42)").is_err());
    env.run("Debug.addLoggingHandler(null); Debug.removeLoggingHandler(null);");
}

#[test]
fn debug_start_log_file_takes_a_clear_flag_and_uses_log_location() {
    let _lock = vm_lock();
    // `TVPStartLogToFile(bool clear)` takes no filename — the stream is
    // `{logLocation}/krkr.console.log` (DebugIntf.cpp:471-506, 240-252).
    let dir = std::env::temp_dir().join(format!(
        "krkr_debug_loc_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let literal = dir.to_string_lossy().replace('\\', "/");
    let file = dir.join("krkr.console.log");

    let env = Env::new();
    env.run(&format!(
        "Debug.logLocation = '{literal}'; Debug.startLogToFile(true); \
         Debug.message('hello');"
    ));
    let written = std::fs::read_to_string(&file).unwrap_or_else(|error| {
        panic!("the reference log file must exist after startLogToFile: {error}")
    });
    assert!(written.contains("hello"), "log: {written:?}");
    assert!(
        written.contains("Logging to "),
        "the reference writes an open header: {written:?}"
    );
    // Zero arguments are valid (clear defaults to false).
    env.run("Debug.startLogToFile(); Debug.startLogToFile(false);");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn debug_log_as_error_honours_the_auto_log_flags() {
    let _lock = vm_lock();
    // `TVPOnError` starts the error log file only when `logToFileOnError` is
    // set, with `clearLogFileOnError` as the clear flag
    // (DebugIntf.cpp:508-513); the method takes no arguments.
    let dir = std::env::temp_dir().join(format!(
        "krkr_debug_err_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let literal = dir.to_string_lossy().replace('\\', "/");
    let file = dir.join("krkr.console.log");

    let env = Env::new();
    env.run(&format!("Debug.logLocation = '{literal}';"));
    env.run("Debug.logAsError();");
    assert!(
        !file.exists(),
        "no auto-log flag means no stream (DebugIntf.cpp:478-481, 509-510)"
    );
    env.run("Debug.logToFileOnError = true; Debug.logAsError();");
    assert!(
        file.exists(),
        "logToFileOnError must open the location's krkr.console.log"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Constants (base/ScriptMgnIntf.cpp:331-361)
// ---------------------------------------------------------------------------

#[test]
fn constants_include_the_image_tag_layer_type_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();
    // `TVPInitTJSScript`'s only non-scalar member is the `imageTagLayerType`
    // dictionary (ScriptMgnIntf.cpp:331-361).
    assert_eq!(
        env.eval_int("imageTagLayerType.alpha.type === ltAlpha ? 1 : 0"),
        1
    );
    assert_eq!(
        env.eval_int("imageTagLayerType.psburn.type === ltPsColorBurn ? 1 : 0"),
        1
    );
    assert_eq!(
        env.eval_int("imageTagLayerType.psexcl.type === ltPsExclusion ? 1 : 0"),
        1
    );
}

// ---------------------------------------------------------------------------
// Clipboard (utils/ClipboardIntf.cpp + impl/ClipboardImpl.cpp)
// ---------------------------------------------------------------------------

#[test]
fn clipboard_unknown_formats_never_consult_the_backend() {
    let _lock = vm_lock();
    let env = Env::new();
    // The reference `switch(format)` default returns false without touching
    // the platform (impl/ClipboardImpl.cpp:11-13), so an unknown code is
    // false even with no backend installed — while a known format raises the
    // A typed error on this host (no clipboard backend).
    assert_eq!(env.eval_int("Clipboard.hasFormat(99)"), 0);
    assert_eq!(env.eval_int("Clipboard.hasFormat(-1)"), 0);
    assert!(env.eval("Clipboard.hasFormat(1)").is_err());
    // `(tjs_int)*param[0]` raises on objects (ClipboardIntf.cpp:57-60).
    assert!(env.eval("Clipboard.hasFormat({})").is_err());
}

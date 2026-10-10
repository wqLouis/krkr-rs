//! Reference-behaviour tests for the `steam`, `steam_api` and
//! `DrawDeviceForSteam` plugin ports.
//!
//! The reference `Steam` class (`reference/cpp/plugins/steam/steamPlugin.cpp`)
//! is a self-contained mock: it links no `libsteam_api` and every member
//! returns a hard-coded value or does nothing. These tests pin the exact mock
//! values, result types (Integer vs Real vs String vs void), the read-only
//! property contract and the registered surface — each with the
//! `reference/cpp/**:line` citation for the behaviour it asserts.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The host is declared before the engine so the process-wide binding is
/// removed before the VM is dropped.
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
            .eval(expression, "steam-test")
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

    fn eval_string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    /// The script `typeof` of an expression (`tjsInterCodeExec.cpp:2601-2628`
    /// returns `"Integer"`/`"Real"`/`"String"`/`"void"`/`"Object"`).
    fn eval_typeof(&self, expression: &str) -> String {
        match self.eval_ok(&format!("typeof ({expression})")) {
            TjsValue::String(value) => value,
            other => panic!("typeof {expression:?} returned {other:?}"),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "steam-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

// ---------------------------------------------------------------------------
// Steam: static mock methods
// ---------------------------------------------------------------------------

/// Reference: every `Steam` static callback writes a hard-coded result —
/// `"Achievement 1"` (`steamPlugin.cpp:65-75`), `int(1)` for
/// set/clear/requestInitialize (`steamPlugin.cpp:46-52,77-91`), `"English"`
/// (`steamPlugin.cpp:94-100`), `int(info.total)` = 1 GiB
/// (`steamPlugin.cpp:109-118`), `"file1.txt"` (`steamPlugin.cpp:120-128`),
/// `int(1)` for delete/copy (`steamPlugin.cpp:131-145`), void for the three
/// screenshot hooks that never assign `*r` (`steamPlugin.cpp:148-161`),
/// `int(0)` broadcasting (`steamPlugin.cpp:164-170`), void hookBroadcasting
/// (`steamPlugin.cpp:172-175`), `int(1)` for subscribe/DLC-installed
/// (`steamPlugin.cpp:178-192`), `int(2)` DLC count (`steamPlugin.cpp:194-200`)
/// and `"DLC1"` (`steamPlugin.cpp:203-209`).
#[test]
fn static_methods_return_the_reference_mock_values() {
    let _lock = vm_lock();
    let env = Env::new();

    // Achievements and language.
    assert_eq!(env.eval_string("Steam.getAchievement()"), "Achievement 1");
    assert_eq!(env.eval_int("Steam.setAchievement('ach1')"), 1);
    assert_eq!(env.eval_int("Steam.clearAchievement('ach1')"), 1);
    assert_eq!(env.eval_string("Steam.getLanguage()"), "English");
    assert_eq!(env.eval_int("Steam.requestInitialize()"), 1);

    // Cloud storage.
    assert_eq!(env.eval_int("Steam.getCloudQuota()"), 1024 * 1024 * 1024);
    assert_eq!(env.eval_string("Steam.getCloudFileInfo(0)"), "file1.txt");
    assert_eq!(env.eval_int("Steam.deleteCloudFile('file1.txt')"), 1);
    assert_eq!(env.eval_int("Steam.copyCloudFile('a', 'b')"), 1);

    // Screenshots: no `*r` assignment in the reference → void.
    assert_eq!(env.eval_ok("Steam.triggerScreenshot()"), TjsValue::Void);
    assert_eq!(env.eval_ok("Steam.hookScreenshots(null)"), TjsValue::Void);
    assert_eq!(
        env.eval_ok("Steam.writeScreenshot(null, '')"),
        TjsValue::Void
    );

    // Broadcasting.
    assert_eq!(env.eval_int("Steam.isBroadcasting()"), 0);
    assert_eq!(env.eval_ok("Steam.hookBroadcasting(null)"), TjsValue::Void);

    // DLC.
    assert_eq!(env.eval_int("Steam.isIsSubscribedApp(1)"), 1);
    assert_eq!(env.eval_int("Steam.ssDlcInstalled(1)"), 1);
    assert_eq!(env.eval_int("Steam.getDLCCount()"), 2);
    assert_eq!(env.eval_string("Steam.getDLCData(0)"), "DLC1");
}

/// Reference: results keep the exact variant of the value the callback
/// writes — `*r = int(...)` is a TJS **Integer**
/// (`steamPlugin.cpp:49,80,88,115,134,142,167,181,189,197`), `*r = "..."` is a
/// String (`steamPlugin.cpp:72,97,125,206`), and a callback that never
/// assigns `*r` leaves the result **void** (`steamPlugin.cpp:148-161`).
/// `tTJSVariant` has no boolean/real spelling for these writes: the `bool`
/// property getters store `tvtInteger` (`tjsVariant.h:617-620`), so nothing
/// on `Steam` is ever a `Real`.
#[test]
fn mock_results_keep_the_reference_variant_types() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.eval_typeof("Steam.getCloudQuota()"), "Integer");
    assert_eq!(env.eval_typeof("Steam.getDLCCount()"), "Integer");
    assert_eq!(env.eval_typeof("Steam.isBroadcasting()"), "Integer");
    assert_eq!(env.eval_typeof("Steam.getLanguage()"), "String");
    assert_eq!(env.eval_typeof("Steam.getAchievement()"), "String");
    assert_eq!(env.eval_typeof("Steam.triggerScreenshot()"), "void");
    // The property getters return `static int` / `static bool`
    // (`steamPlugin.cpp:53,59,103,129,201`) → Integer, never Real.
    assert_eq!(env.eval_typeof("Steam.initialized"), "Integer");
    assert_eq!(env.eval_typeof("Steam.cloudEnabled"), "Integer");
    assert_eq!(env.eval_typeof("Steam.dlcCount"), "Integer");
}

/// Reference: `Steam` static properties are read through the registered
/// getters — `initialized` returns `1` (`steamPlugin.cpp:53`),
/// `achievementsCount` `0` (`steamPlugin.cpp:59`), `cloudEnabled` `true`,
/// which `tTJSVariant(bool)` stores as Integer `1` (`steamPlugin.cpp:103` +
/// `tjsVariant.h:617-620`), `cloudFileCount` `0` (`steamPlugin.cpp:129`) and
/// `dlcCount` through `_getDlcsCount()` → `0` (`steamPlugin.cpp:201`), all
/// registered at `steamPlugin.cpp:226-230`.
#[test]
fn static_properties_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.eval_int("Steam.initialized"), 1);
    assert_eq!(env.eval_int("Steam.achievementsCount"), 0);
    assert_eq!(env.eval_int("Steam.cloudEnabled"), 1);
    assert_eq!(env.eval_int("Steam.cloudFileCount"), 0);
    assert_eq!(env.eval_int("Steam.dlcCount"), 0);
}

/// Reference: the callbacks take `tjs_int`/`tTJSVariant **` parameters but
/// never inspect `numparams` or `param` (`steamPlugin.cpp:46-47,65-66,94-95,
/// 120-121,203-204`), so arity is irrelevant: `getAchievement()` and
/// `getDLCData()` succeed without their argument and extra arguments change
/// nothing.
#[test]
fn static_members_ignore_their_arguments() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference callbacks drop `n`; every call returns the same mock.
    assert_eq!(env.eval_string("Steam.getAchievement(0)"), "Achievement 1");
    assert_eq!(
        env.eval_string("Steam.getAchievement('other')"),
        "Achievement 1"
    );
    assert_eq!(env.eval_string("Steam.getCloudFileInfo(99)"), "file1.txt");
    assert_eq!(env.eval_string("Steam.getDLCData(7)"), "DLC1");
    // No argument-count check anywhere in the reference class: calling
    // without the documented argument succeeds and returns the same mock.
    assert_eq!(env.eval_string("Steam.getAchievement()"), "Achievement 1");
    assert_eq!(env.eval_string("Steam.getDLCData()"), "DLC1");
    assert_eq!(env.eval_int("Steam.setAchievement()"), 1);
    assert_eq!(env.eval_int("Steam.getDLCCount('ignored')"), 2);
}

/// Reference: the properties are registered with ncbind's read-only
/// `Property(name, getter, int())` overload (`steamPlugin.cpp:226-230`),
/// which stores a **null setter** (`ncbind.hpp:1727`), so `PropSet` answers
/// `TJS_E_ACCESSDENYED` (`ncbind.hpp:1477`) — a write raises and does not
/// change the getter's value. The reference's `setInitialized` /
/// `setAchievementsCount` / `setCloudEnabled` functions
/// (`steamPlugin.cpp:54-63,104-107`) are *not* in the registration block, so
/// they cannot be reached as setters either.
#[test]
fn static_properties_are_read_only_and_writes_do_not_stick() {
    let _lock = vm_lock();
    let env = Env::new();

    for member in [
        "initialized",
        "achievementsCount",
        "cloudEnabled",
        "cloudFileCount",
        "dlcCount",
    ] {
        let message = env.error(&format!("Steam.{member} = 1"));
        assert!(!message.is_empty(), "writing Steam.{member} should raise");
    }
    // A rejected write has no side effect: the getters still answer the
    // reference mock values (`steamPlugin.cpp:53,59,103,129,201`).
    assert_eq!(env.eval_int("Steam.initialized"), 1);
    assert_eq!(env.eval_int("Steam.achievementsCount"), 0);
    assert_eq!(env.eval_int("Steam.cloudEnabled"), 1);
    assert_eq!(env.eval_int("Steam.cloudFileCount"), 0);
    assert_eq!(env.eval_int("Steam.dlcCount"), 0);
}

/// Reference: `NCB_REGISTER_CLASS(Steam)` registers exactly the eighteen
/// `RawCallback`s and five `Property`s at `steamPlugin.cpp:218-253`. The
/// C++ helpers `setInitialized`, `setAchievementsCount`, `setCloudEnabled`,
/// `getInitialized`, `getAchievementsCount`, `getCloudEnabled`,
/// `getCloudFileCount` and `_getDlcsCount` (`steamPlugin.cpp:53-63,103-107,
/// 129,201`) are referenced only through the `Property` getters — they are
/// **not** script members under their own names.
#[test]
fn unregistered_cpp_helpers_are_not_script_members() {
    let _lock = vm_lock();
    let env = Env::new();

    for member in [
        "getInitialized",
        "setInitialized",
        "getAchievementsCount",
        "setAchievementsCount",
        "getCloudEnabled",
        "setCloudEnabled",
        "getCloudFileCount",
        "_getDlcsCount",
    ] {
        assert!(
            env.eval(&format!("Steam.{member}")).is_err(),
            "Steam.{member} is not in the reference registration block \
             (steamPlugin.cpp:218-253)"
        );
    }
}

// ---------------------------------------------------------------------------
// Steam: class shape
// ---------------------------------------------------------------------------

/// Reference: `NCB_REGISTER_CLASS(Steam)` declares no `Constructor()`
/// (`steamPlugin.cpp:218-253`), so ncbind installs its dummy constructor,
/// which raises `TJS_E_NOTIMPL` (`reference/cpp/core/plugin/ncbind.hpp:1948`,
/// `1951-1956`) — `new Steam()` fails. Every member still carries
/// `TJS_STATICMEMBER` (`steamPlugin.cpp:220-253`), so the class object keeps
/// the whole surface.
#[test]
fn steam_has_no_constructor_and_keeps_its_statics() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference's dummy constructor returns `TJS_E_NOTIMPL`
    // (`ncbind.hpp:1948`), so `new Steam()` raises.
    assert!(
        env.eval("new Steam()").is_err(),
        "the reference's dummy constructor must raise"
    );
    assert!(
        env.eval("new Steam(1, 2, 3)").is_err(),
        "arguments cannot reach a constructor that does not exist"
    );
    // The class-level surface is unaffected.
    assert_eq!(env.eval_string("Steam.getLanguage()"), "English");
    assert_eq!(env.eval_int("Steam.initialized"), 1);
}

// ---------------------------------------------------------------------------
// steam_api
// ---------------------------------------------------------------------------

/// Reference: `steam_api` registers only `Constructor()` — the class body
/// holds a plain C++ field `vserion` that is never bound to TJS
/// (`reference/cpp/plugins/steam/steam_api.cpp:7-12`). The class
/// instantiates; neither `version` nor the reference's misspelled `vserion`
/// is script surface.
#[test]
fn steam_api_is_an_empty_constructor_only_class() {
    let _lock = vm_lock();
    let env = Env::new();

    // The class is instantiable (the reference registers only `Constructor()`).
    env.run("var api = new steam_api();");
    for member in ["version", "vserion"] {
        assert!(
            env.eval(&format!("api.{member}")).is_err(),
            "steam_api must not expose a {member} member \
             (steam_api.cpp:7-12 binds it to C++ only)"
        );
    }
}

// ---------------------------------------------------------------------------
// DrawDeviceForSteam
// ---------------------------------------------------------------------------

/// Reference: `DrawDeviceForSteam` is registered with `Factory` and one
/// read-only `interface` property (`DrawDeviceForSteam.cpp:412-415`) whose
/// getter returns the main window's raw draw-device pointer reinterpreted as
/// `tjs_int64` (`DrawDeviceForSteam.cpp:21-24`). krkr-rs has no host
/// draw-device object — the core `Window.drawDevice` reports the same typed
/// failure (`natives/visual/window.rs`) — so the port raises a typed D11
/// error naming the member instead of fabricating a pointer value.
#[test]
fn draw_device_for_steam_is_registered_with_a_typed_interface_error() {
    let _lock = vm_lock();
    let env = Env::new();

    // The proxy is instantiable...
    env.run("var d = new DrawDeviceForSteam();");
    // ... and `interface` reports that the host has no draw-device object
    // instead of fabricating a pointer (the same contract as Window.drawDevice).
    let message = env.error("d.interface");
    assert!(
        message.contains("DrawDeviceForSteam.interface"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("draw-device"),
        "unexpected message: {message}"
    );
    // A second, instance-level write attempt must not silently succeed either:
    // `interface` is registered as a getter-only property
    // (`DrawDeviceForSteam.cpp:414`, null setter via `ncbind.hpp:1727`).
    assert!(env.eval("d.interface = 1").is_err());
}

// ---------------------------------------------------------------------------
// Surface
// ---------------------------------------------------------------------------

/// Reference: the complete script surface is the `NCB_REGISTER_CLASS(Steam)`
/// block (`steamPlugin.cpp:218-253`) plus the `steam_api` constructor
/// (`steam_api.cpp:12`) and the `DrawDeviceForSteam` factory/property
/// (`DrawDeviceForSteam.cpp:412-415`) — nothing else. Anything outside that
/// set must raise.
#[test]
fn every_reference_member_is_registered() {
    let _lock = vm_lock();
    let env = Env::new();

    for member in [
        "getAchievement",
        "setAchievement",
        "clearAchievement",
        "getLanguage",
        "requestInitialize",
        "getCloudQuota",
        "getCloudFileInfo",
        "deleteCloudFile",
        "copyCloudFile",
        "triggerScreenshot",
        "hookScreenshots",
        "writeScreenshot",
        "isBroadcasting",
        "hookBroadcasting",
        "isIsSubscribedApp",
        "ssDlcInstalled",
        "getDLCCount",
        "getDLCData",
        "initialized",
        "achievementsCount",
        "cloudEnabled",
        "cloudFileCount",
        "dlcCount",
    ] {
        let probe = format!("Steam.{member}");
        assert!(env.eval(&probe).is_ok(), "{member} is not registered");
    }
    // No member beyond the reference block (steamPlugin.cpp:218-253).
    for probe in ["Steam.onAchievement", "Steam.getAchievementInfo"] {
        assert!(
            env.eval(probe).is_err(),
            "{probe} is not part of the reference registration"
        );
    }
    // `steam_api` is constructor-only: it instantiates but has no members.
    env.run("var api = new steam_api();");
    assert!(
        env.eval("api.version").is_err(),
        "steam_api must not expose a version member"
    );
    env.run("var d = new DrawDeviceForSteam();");
    // `interface` raises, but the member is registered (the error names it).
    assert!(
        env.error("d.interface").contains("interface"),
        "DrawDeviceForSteam.interface missing"
    );
}

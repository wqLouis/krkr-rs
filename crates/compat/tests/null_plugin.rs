//! Integration tests for the null plugin object's **global handling**:
//! `Plugins.link` must never shadow a binding the game needs
//! and `Plugins.unlink` must never delete one it did not install.
//!
//! The three properties under test:
//!
//! * an existing `global[stem]` (a native class or a script value) is kept —
//!   the null path never clobbers it;
//! * `unlink` clears only the global the link itself installed, and only
//!   while it still holds the null object — a script that claimed the stem
//!   keeps its value;
//! * a null global survives the full link → unlink → link round trip, and
//!   built-in links install no null global at all.

mod common;

use common::vm_lock;
use compat::{CompatHost, plugin_stem};
use tjs::{Tjs2Engine, TjsValue};

/// The nine DLLs the shipped game links (`initialize.tjs:127` trace): all
/// resolve to built-ins, so none of them may install a null global.
const GAME_LINKS: &[&str] = &[
    "csvParser.dll",
    "extrans.dll",
    "fstat.dll",
    "getSample.dll",
    "KAGParserEx.dll",
    "layerExDraw.dll",
    "win32dialog.dll",
    "windowEx.dll",
    "wuvorbis.dll",
];

/// A bound VM + host.
///
/// The engine is **boxed**: `CompatHost::install` records the `Tjs2Engine`
/// *address* in the process-wide binding, so returning it by value would
/// leave that pointer dangling when the struct moves (the binding is read on
/// every `Plugins.link` of a non-built-in). The host is declared first so its
/// `Drop` unbinds before the VM is destroyed — the same shape as the `Env` in
/// `tests/sound.rs`.
fn install() -> (Box<Tjs2Engine>, CompatHost) {
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut host = CompatHost::new();
    host.install(&engine).expect("install compat surface");
    (engine, host)
}

/// `Rect.dll` is not a ported plugin and its stem collides with the native
/// `Rect` class: the link must record the name without shadowing the class,
/// and the unlink must not delete the class global.
#[test]
fn null_link_never_shadows_a_native_class_global() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script("Plugins.link('Rect.dll');", "test")
        .expect("link a plugin whose stem collides with the Rect class");
    assert_eq!(host.registry().linked(), vec!["Rect.dll".to_string()]);
    engine
        .exec_script("var r = new Rect(10, 20, 50, 80);", "test")
        .expect("the native Rect class survived the link");
    assert_eq!(
        engine.eval("r.left", "test").unwrap(),
        TjsValue::Integer(10)
    );

    // Unlink drops the getList entry but must leave the class alone: this
    // link never installed a global (the class was already there).
    engine
        .exec_script("Plugins.unlink('Rect.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
    engine
        .exec_script("r = new Rect(1, 2, 3, 4);", "test")
        .expect("unlink must not delete a global the link never installed");
    assert_eq!(engine.eval("r.left", "test").unwrap(), TjsValue::Integer(1));
}

/// The same guarantee for a plain script global the game defines before it
/// links (or links a DLL whose stem happens to match one of its own).
#[test]
fn null_link_never_shadows_a_script_global() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script("var shared = 42; Plugins.link('shared.dll');", "test")
        .expect("link next to a script global");
    assert_eq!(host.registry().linked(), vec!["shared.dll".to_string()]);
    assert_eq!(
        engine.eval("shared", "test").unwrap(),
        TjsValue::Integer(42)
    );

    engine
        .exec_script("Plugins.unlink('shared.dll');", "test")
        .expect("unlink");
    assert_eq!(
        engine.eval("shared", "test").unwrap(),
        TjsValue::Integer(42)
    );
}

/// A stem that is free gets the null object; `unlink` removes the member
/// outright (not `= void`, so `typeof` reports it gone) and a re-link
/// installs a fresh null object.
#[test]
fn null_global_round_trips_through_unlink_and_relink() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script("Plugins.link('temp.dll');", "test")
        .expect("first link installs the null object");
    assert_eq!(
        engine.eval("typeof global['temp']", "test").unwrap(),
        TjsValue::String("Object".into())
    );
    engine
        .exec_script("temp.anything(1, 2);", "test")
        .expect("the null object accepts calls");

    engine
        .exec_script("Plugins.unlink('temp.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
    assert_eq!(
        engine.eval("typeof global['temp']", "test").unwrap(),
        TjsValue::String("undefined".into())
    );

    engine
        .exec_script("Plugins.link('temp.dll');", "test")
        .expect("re-link installs a fresh null object");
    assert_eq!(
        engine.eval("typeof global['temp']", "test").unwrap(),
        TjsValue::String("Object".into())
    );
    engine
        .exec_script("temp.anything(1, 2);", "test")
        .expect("the reinstalled null object accepts calls");
}

/// A script that claims the stem after the link keeps its value: `unlink`
/// only clears the null object it installed, and a fresh link on the taken
/// stem keeps the value too.
#[test]
fn unlink_keeps_a_global_the_script_wrote_over() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script("Plugins.link('round.dll');", "test")
        .expect("link installs the null object");
    engine
        .exec_script("global['round'] = 7;", "test")
        .expect("the script claims the stem");
    engine
        .exec_script("Plugins.unlink('round.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
    assert_eq!(
        engine.eval("global['round']", "test").unwrap(),
        TjsValue::Integer(7)
    );

    // Re-linking a taken stem keeps the script's binding, and unlinking it
    // again (this link installed nothing) still keeps it.
    engine
        .exec_script("Plugins.link('round.dll');", "test")
        .expect("re-link on a taken stem");
    assert_eq!(
        engine.eval("global['round']", "test").unwrap(),
        TjsValue::Integer(7)
    );
    engine
        .exec_script("Plugins.unlink('round.dll');", "test")
        .expect("unlink");
    assert_eq!(
        engine.eval("global['round']", "test").unwrap(),
        TjsValue::Integer(7)
    );
}

/// Two plugin names with one stem (`stem.dll` / `stem.tpm`): the first link
/// owns the null global, the second finds it taken and installs nothing —
/// so unlinking the second keeps the global while unlinking the first
/// clears it.
#[test]
fn same_stem_links_share_one_global_and_only_the_owner_clears_it() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script(
            "Plugins.link('stem.dll'); Plugins.link('stem.tpm');",
            "test",
        )
        .expect("link two names with the same stem");
    assert_eq!(
        host.registry().linked(),
        vec!["stem.dll".to_string(), "stem.tpm".to_string()]
    );
    assert_eq!(
        engine.eval("typeof global['stem']", "test").unwrap(),
        TjsValue::String("Object".into())
    );

    engine
        .exec_script("Plugins.unlink('stem.tpm');", "test")
        .expect("unlink the non-installing link");
    assert_eq!(
        engine.eval("typeof global['stem']", "test").unwrap(),
        TjsValue::String("Object".into())
    );

    engine
        .exec_script("Plugins.unlink('stem.dll');", "test")
        .expect("unlink the owning link");
    assert_eq!(
        engine.eval("typeof global['stem']", "test").unwrap(),
        TjsValue::String("undefined".into())
    );
}

/// Every DLL the shipped game links resolves to a built-in, so no null
/// object may exist at any of their stems. The guard short-circuits before
/// reading a possibly-absent member (a plain read would raise).
#[test]
fn builtin_game_links_install_no_null_globals() {
    let _lock = vm_lock();
    let (engine, host) = install();

    for name in GAME_LINKS {
        engine
            .exec_script(&format!("Plugins.link('{name}');"), "test")
            .expect("link a built-in plugin");
    }
    let linked: Vec<String> = GAME_LINKS.iter().map(|name| name.to_string()).collect();
    assert_eq!(host.registry().linked(), linked);

    for name in GAME_LINKS {
        let stem = plugin_stem(name);
        let expr = format!(
            "typeof global['{stem}'] != 'Object' || !(global['{stem}'] instanceof \"NullPlugin\")"
        );
        assert_eq!(
            engine.eval(&expr, "test").unwrap(),
            TjsValue::Integer(1),
            "the stem of {name} must not be a null plugin object"
        );
    }
}

/// One plugin, two spellings: `normalize_plugin_name` folds the case for
/// lookup, so the second link is idempotent — the list keeps the first
/// casing and the null global keeps it too (there is no second install).
#[test]
fn case_variants_of_one_plugin_name_link_once() {
    let _lock = vm_lock();
    let (engine, host) = install();

    engine
        .exec_script(
            "Plugins.link('Case.dll'); Plugins.link('case.DLL');",
            "test",
        )
        .expect("link the same plugin under two spellings");
    assert_eq!(host.registry().linked(), vec!["Case.dll".to_string()]);
    assert_eq!(
        engine.eval("typeof global['Case']", "test").unwrap(),
        TjsValue::String("Object".into())
    );

    engine
        .exec_script("Plugins.unlink('case.dll');", "test")
        .expect("unlink under the other spelling");
    assert!(host.registry().linked().is_empty());
    assert_eq!(
        engine.eval("typeof global['Case']", "test").unwrap(),
        TjsValue::String("undefined".into())
    );
}

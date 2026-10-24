//! Integration tests for the native-plugin architecture and the null plugin.

mod common;

use common::vm_lock;
use compat::{
    CompatCtx, CompatError, NativePlugin, PluginRegistry, normalize_plugin_name, plugin_stem,
};
use tjs::{NativeClassBuilder, Tjs2Engine, TjsValue};

/// Top-level classes the reference registers as core natives before any game
/// script runs (`reference/cpp/core/base/ScriptMgnIntf.cpp:484-542`).
const CORE_STARTUP_CLASSES: &[&str] = &[
    "Debug",                // :484
    "Font",                 // :485
    "Layer",                // :486
    "CDDASoundBuffer",      // :487-488
    "MIDISoundBuffer",      // :489-490
    "Timer",                // :491
    "AsyncTrigger",         // :492
    "System",               // :493
    "Storages",             // :494
    "Plugins",              // :495
    "VideoOverlay",         // :496
    "Pad",                  // :497
    "Clipboard",            // :498
    "Scripts",              // :499-500
    "Rect",                 // :501
    "Bitmap",               // :502
    "ImageFunction",        // :503-504
    "BitmapLayerTreeOwner", // :505-506
    "KAGParser",            // :509
    "WaveSoundBuffer",      // :513-514
    "Window",               // :523-524
    "MenuItem",             // :542
];

struct StubPlugin;

impl NativePlugin for StubPlugin {
    fn name(&self) -> &str {
        "stub.dll"
    }

    fn register(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}

/// A built-in that actually registers a native class, so a duplicate
/// registration is observable (the VM rejects duplicate class names).
struct RegisteringPlugin;

impl NativePlugin for RegisteringPlugin {
    fn name(&self) -> &str {
        "reg.dll"
    }

    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        ctx.register_class(&NativeClassBuilder {
            name: "RegClass",
            methods: vec![],
            properties: vec![],
        })
    }
}

#[test]
fn normalize_and_stem() {
    assert_eq!(
        normalize_plugin_name(r"C:\game\Plugin\Extrans.DLL"),
        "extrans.dll"
    );
    assert_eq!(plugin_stem(r"C:\game\Plugin\Extrans.DLL"), "Extrans");
    assert_eq!(plugin_stem("noext"), "noext");
    assert_eq!(plugin_stem("arc.xp3>plug.dll"), "plug");
}

#[test]
fn registry_finds_builtins_case_insensitively() {
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(StubPlugin));
    assert!(registry.is_builtin("STUB.DLL"));
    assert!(registry.is_builtin(r"c:\x\stub.dll"));
    assert!(!registry.is_builtin("other.dll"));
    assert!(registry.plugin("stub.dll").is_some());
    assert!(registry.linked().is_empty());
}

#[test]
fn ctx_is_absent_before_install() {
    let host = compat::CompatHost::new();
    assert!(host.ctx().is_err(), "an uninstalled host has no context");
    assert!(!host.is_bound());
}

#[test]
fn link_unlink_link_is_idempotent_for_builtins() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(RegisteringPlugin));
    let mut host = compat::CompatHost::with_registry(registry);
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script("Plugins.link('reg.dll');", "test")
        .expect("first link registers the class");
    engine
        .exec_script("Plugins.unlink('reg.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
    // Re-linking must not re-register the class (the VM rejects duplicates).
    engine
        .exec_script("Plugins.link('reg.dll');", "test")
        .expect("re-link is idempotent");
    assert_eq!(host.registry().linked(), vec!["reg.dll".to_string()]);
}

#[test]
fn linked_list_preserves_insertion_order_and_case() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script(
            "Plugins.link('First.dll'); Plugins.link('second.dll');",
            "test",
        )
        .expect("link two null plugins");
    assert_eq!(
        host.registry().linked(),
        vec!["First.dll".to_string(), "second.dll".to_string()]
    );
    // The null global keeps the caller's casing.
    engine
        .eval("First.someMember", "test")
        .expect("original-case global exists");
}

#[test]
fn link_installs_null_object_for_unknown_plugin() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script("Plugins.link('totally_unknown.dll');", "test")
        .expect("link unknown plugin");

    // A call to an absent member is accepted and does nothing.
    engine
        .exec_script("totally_unknown.someMethod(1, 2, 3);", "test")
        .expect("null plugin accepts calls");
    // Reading a member does not throw either.
    engine
        .eval("totally_unknown.someMember", "test")
        .expect("null plugin accepts reads");
    // Reading a member and then calling it also works.
    engine
        .exec_script("var m = totally_unknown.otherMethod; m(9);", "test")
        .expect("null plugin accepts a read-then-call");
    // Writing an absent member is also accepted.
    engine
        .eval("totally_unknown.someMember = 3", "test")
        .expect("null plugin accepts writes");
    // `getList` reports the linked (normalized) name.
    assert_eq!(
        engine.eval("Plugins.getList()[0]", "test").unwrap(),
        TjsValue::String("totally_unknown.dll".into())
    );
    // The reference `getList` has no arity check, so extra arguments are
    // ignored (`PluginImpl.cpp:332-345`).
    assert_eq!(
        engine.eval("Plugins.getList(1, 2)[0]", "test").unwrap(),
        TjsValue::String("totally_unknown.dll".into())
    );
    // Unlink forgets it and reports success.
    engine
        .exec_script("Plugins.unlink('totally_unknown.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
}

/// Table completeness: every DLL the shipped game links
/// plus the reference `NCB_MODULE_NAME` aliases of ported plugins must
/// resolve to a built-in — anything else would take the null path and
/// shadow the real emulation.
///
/// Verified against `reference/cpp/plugins/**` (`NCB_MODULE_NAME` and the
/// `CMakeLists.txt` source list): `layerExMovie.cpp` → `layerExMovie.dll`,
/// `layerExPerspective.cpp` → `perspective.dll`, `steamPlugin.cpp` →
/// `krkrsteam.dll`, `steam_api.cpp` → `steam_api.dll`,
/// `DrawDeviceForSteam.cpp` → `DrawDeviceForSteam.dll`.
#[test]
fn builtin_table_covers_the_games_links_and_reference_aliases() {
    let registry = PluginRegistry::builtin();
    for name in [
        // The nine links in the shipped game's `system/*.tjs`.
        "csvParser.dll",
        "extrans.dll",
        "fstat.dll",
        "getSample.dll",
        "KAGParserEx.dll",
        "layerExDraw.dll",
        "win32dialog.dll",
        "windowEx.dll",
        "wuvorbis.dll",
        // Reference aliases of ported plugins.
        "layerExMovie.dll",
        "perspective.dll",
        "krkrsteam.dll",
        "steam_api.dll",
        "DrawDeviceForSteam.dll",
        // Case and path forms scripts actually pass.
        "CSVParser.DLL",
        r"C:\game\plugin\WindowEx.dll",
    ] {
        assert!(
            registry.is_builtin(name),
            "{name} must resolve to a built-in"
        );
    }
    assert!(!registry.is_builtin("not_a_real_plugin.dll"));
    assert!(!registry.is_builtin("AlphaMovie.dll"));
}

/// The reference registers these classes in its startup block
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:484-542`), so each must be on
/// the VM immediately after `CompatHost::install` and before any
/// `Plugins.link`.
#[test]
fn core_startup_classes_exist_before_any_plugin_link() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");
    assert!(
        host.registry().linked().is_empty(),
        "this test must not link a plugin"
    );
    for name in CORE_STARTUP_CLASSES {
        let value = engine
            .eval(&format!("typeof {name}"), "plugin_test")
            .unwrap_or_else(|error| panic!("eval typeof {name} failed: {error}"));
        assert_eq!(
            value,
            TjsValue::String("Object".to_string()),
            "{name} must exist as a core startup class"
        );
    }
}

/// `KAGParser` is core-registered (`reference/cpp/core/base/ScriptMgnIntf.cpp:509`)
/// and the plugin is an empty placeholder
/// (`reference/cpp/plugins/KAGParser/kagparserex.cpp:1-8`), so both DLL names
/// link without registering the class a second time. A duplicate registration
/// would make the link itself raise.
#[test]
fn kagparser_plugin_names_are_empty_placeholders() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");
    for name in [
        "KAGParser.dll",
        "KAGParserEx.dll",
        "KAGParser.dll",
        "KAGParserEx.dll",
    ] {
        engine
            .exec_script_discard(&format!("Plugins.link('{name}');"), "plugin_test")
            .unwrap_or_else(|error| panic!("Plugins.link({name:?}) failed: {error}"));
    }
    assert_eq!(
        host.registry().linked(),
        vec!["KAGParser.dll".to_string(), "KAGParserEx.dll".to_string()]
    );
    engine
        .exec_script_discard("class AfterParserLink extends KAGParser { }", "plugin_test")
        .expect("the core class must still be usable after both links");
}

/// A built-in whose per-frame hook counts its invocations.
struct PollingPlugin {
    polls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl NativePlugin for PollingPlugin {
    fn name(&self) -> &str {
        "poll.dll"
    }

    fn register(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.polls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// A plugin that no script has linked has no script-visible surface, so its
/// per-frame hook must not run; the first `Plugins.link` makes it active and
/// the hook then runs once per poll.
#[test]
fn poll_runs_only_for_activated_plugins() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(PollingPlugin {
        polls: polls.clone(),
    }));
    let mut host = compat::CompatHost::with_registry(registry);
    host.install(&engine).expect("install compat surface");
    let mut ctx = host.ctx().expect("bound context");

    host.registry().poll(&mut ctx).expect("idle poll");
    assert_eq!(
        polls.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an unlinked plugin's hook must not run"
    );

    host.registry()
        .activate("poll.dll", &mut ctx)
        .expect("link the plugin");
    host.registry().poll(&mut ctx).expect("active poll");
    assert_eq!(
        polls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the linked plugin's hook ran once"
    );
}

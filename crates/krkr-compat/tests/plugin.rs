//! Integration tests for the native-plugin architecture and the null plugin.

mod common;

use common::vm_lock;
use krkr_compat::{
    CompatCtx, CompatError, NativePlugin, PluginRegistry, normalize_plugin_name, plugin_stem,
};
use tjs2_sys::{Tjs2Engine, TjsValue};

struct StubPlugin;

impl NativePlugin for StubPlugin {
    fn name(&self) -> &str {
        "stub.dll"
    }

    fn register(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
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
fn link_installs_null_object_for_unknown_plugin() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = krkr_compat::CompatHost::new();
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
    // Unlink forgets it.
    engine
        .exec_script("Plugins.unlink('totally_unknown.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
}

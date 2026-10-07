//! Integration tests for the native-plugin architecture and the null plugin.

mod common;

use common::vm_lock;
use compat::{
    CompatCtx, CompatError, NativePlugin, PluginRegistry, normalize_plugin_name, plugin_stem,
};
use tjs::{NativeClassBuilder, Tjs2Engine, TjsValue};

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
    // Unlink forgets it and reports success.
    engine
        .exec_script("Plugins.unlink('totally_unknown.dll');", "test")
        .expect("unlink");
    assert!(host.registry().linked().is_empty());
}

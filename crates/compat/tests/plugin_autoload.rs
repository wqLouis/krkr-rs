//! Integration tests for the `.tpm` auto-load path
//! ([`PluginRegistry::autoload`]).
//!
//! The reference `tvpLoadPlugins` scans the exe directory, `<exe>/system` and
//! `<exe>/plugin` for `*.tpm`, sorts the combined list by file name and loads
//! each in turn (`reference/cpp/core/plugin/PluginImpl.cpp:148-177`, called
//! from `reference/cpp/core/environ/Application.cpp:342`). The scan belongs to
//! the composition root (tested in `crates/app`); this crate receives the
//! ordered file names and links them through the same registry path as
//! `Plugins.link`.

mod common;

use common::vm_lock;
use compat::{CompatCtx, CompatError, NativePlugin, PluginRegistry};
use tjs::{NativeClassBuilder, Tjs2Engine, TjsValue};

thread_local! {
    static CAPTURED_LOGS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A `log` sink that records every record on the calling thread.
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS.with(|logs| {
            logs.borrow_mut()
                .push(format!("{} {}", record.level(), record.args()));
        });
    }

    fn flush(&self) {}
}

/// Install the capturing sink once per test binary, clear this thread's
/// buffer, run `f`, and return its result plus the thread's log lines.
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = log::set_boxed_logger(Box::new(CaptureLogger));
        log::set_max_level(log::LevelFilter::Debug);
    });
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
    let result = f();
    let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    (result, logs)
}

/// A built-in whose DLL name is what `convert.tpm` must resolve to.
struct ConvertPlugin;

impl NativePlugin for ConvertPlugin {
    fn name(&self) -> &str {
        "convert.dll"
    }

    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        ctx.register_class(&NativeClassBuilder {
            name: "ConvertClass",
            methods: vec![],
            properties: vec![],
        })
    }
}

// The auto-load replaces a trailing `.tpm` with `.dll` before resolving
// (`reference/cpp/core/plugin/PluginImpl.cpp:113-133`), so `convert.tpm`
// activates the `convert.dll` built-in and its class is registered.
#[test]
fn autoload_resolves_tpm_as_dll_builtin() {
    let _lock = vm_lock();
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(ConvertPlugin));
    let mut host = compat::CompatHost::with_registry(registry);
    host.install(&engine).expect("install compat surface");

    // The class is not on the VM until the plugin is linked.
    assert_eq!(
        engine
            .eval("typeof global.ConvertClass", "test")
            .expect("probe"),
        TjsValue::String("undefined".into())
    );

    let mut ctx = host.ctx().expect("bound context");
    host.registry()
        .autoload(&["convert.tpm".to_string()], &mut ctx)
        .expect("autoload");

    assert_eq!(host.registry().linked(), vec!["convert.dll".to_string()]);
    assert_eq!(
        engine.eval("typeof ConvertClass", "test").expect("probe"),
        TjsValue::String("Object".into())
    );
}

// An unported `.tpm` takes the null-plugin path and warns exactly once (the
// documented D14 divergence: the reference tries to load the DLL and creates
// no global on failure). The reference's `(info) Loading <name>` line is
// emitted first (`reference/cpp/core/plugin/PluginImpl.cpp:173-174`).
#[test]
fn autoload_installs_null_for_unported_and_warns_once() {
    let _lock = vm_lock();
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    let (_, logs) = capture_logs(|| {
        let mut ctx = host.ctx().expect("bound context");
        // The same file found in two scanned directories must warn only once.
        host.registry()
            .autoload(
                &["Missing.tpm".to_string(), "Missing.tpm".to_string()],
                &mut ctx,
            )
            .expect("autoload");
    });

    // The null object is `global.Missing` (the `.tpm` name with the extension
    // replaced by `.dll`); the script sees an object, not `undefined`.
    assert_eq!(
        engine.eval("typeof global.Missing", "test").expect("probe"),
        TjsValue::String("Object".into())
    );
    assert!(
        host.registry()
            .linked()
            .iter()
            .any(|name| name == "Missing.dll")
    );

    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("(info) Loading Missing.tpm"))
            .count(),
        2,
        "the reference load line is emitted per occurrence: {logs:?}"
    );
    let warnings: Vec<&String> = logs
        .iter()
        .filter(|line| line.starts_with("WARN"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one warning for one unported plugin: {warnings:?}"
    );
    assert!(
        warnings[0].contains("null plugin path"),
        "warning was: {}",
        warnings[0]
    );
}

// The native `Plugins.link` path is unchanged: it still resolves a built-in by
// its DLL name and still takes the null path for an unknown name. `autoload`
// reuses the same registry activation, so the native surface gains no member.
#[test]
fn plugins_link_path_is_unchanged() {
    let _lock = vm_lock();
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(ConvertPlugin));
    let mut host = compat::CompatHost::with_registry(registry);
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script("Plugins.link('convert.dll');", "test")
        .expect("link the built-in by DLL name");
    engine
        .exec_script("Plugins.link('nope.dll');", "test")
        .expect("link an unknown name");
    assert_eq!(
        host.registry().linked(),
        vec!["convert.dll".to_string(), "nope.dll".to_string()]
    );
    assert_eq!(
        engine.eval("typeof ConvertClass", "test").expect("probe"),
        TjsValue::String("Object".into())
    );
    assert_eq!(
        engine.eval("typeof global.nope", "test").expect("probe"),
        TjsValue::String("Object".into())
    );
}

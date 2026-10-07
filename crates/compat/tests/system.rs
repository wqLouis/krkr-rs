//! Integration tests for the `System` native class and the TVP global
//! constants.
//!
//! Every test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`], because the engine → context binding is
//! process-wide.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// Build a VM with the full compat surface installed and run `f` against it.
///
/// The engine is a local (never moved after install), so the binding's stored
/// address stays valid; `host` is declared after the engine and therefore
/// drops first, releasing the binding while the VM is still alive.
fn with_vm(f: impl FnOnce(&Tjs2Engine)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install compat surface");
    f(&engine);
}

#[test]
fn tvp_global_constants_are_defined() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(e.eval("ltOpaque", "t").unwrap(), TjsValue::Integer(1));
        assert_eq!(e.eval("ltAddAlpha", "t").unwrap(), TjsValue::Integer(12));
        assert_eq!(e.eval("ssShift", "t").unwrap(), TjsValue::Integer(1));
        assert_eq!(e.eval("mbLeft", "t").unwrap(), TjsValue::Integer(0));
        assert_eq!(e.eval("faAnyFile", "t").unwrap(), TjsValue::Integer(0x3f));
        assert_eq!(e.eval("crHandPoint", "t").unwrap(), TjsValue::Integer(-21));
        assert_eq!(
            e.eval("clHighlight", "t").unwrap(),
            TjsValue::Integer(0x3399ff)
        );
    });
}

#[test]
fn arguments_round_trip_with_reference_prefix_semantics() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script("System.setArgument('-argcheck', 'x');", "t")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-argcheck')", "t").unwrap(),
            TjsValue::String("x".into())
        );
        // A strict prefix of a stored name does not match.
        assert_eq!(
            e.eval("System.getArgument('-arg', 'dflt')", "t").unwrap(),
            TjsValue::String("dflt".into())
        );
        // `setArgument` rewrites the full-name entry.
        e.exec_script("System.setArgument('-argcheck', 'y');", "t")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-argcheck')", "t").unwrap(),
            TjsValue::String("y".into())
        );
        // A miss returns void, or the type-preserving default.
        assert_eq!(
            e.eval("System.getArgument('-missing')", "t").unwrap(),
            TjsValue::Void
        );
        assert_eq!(
            e.eval("System.getArgument('-missing', 7)", "t").unwrap(),
            TjsValue::Integer(7)
        );
        assert!(e.eval("System.setArgument('only-one')", "t").is_err());
        assert!(e.eval("System.getArgument()", "t").is_err());
    });
}

#[test]
fn to_actual_color_maps_system_colors_and_passes_rgb() {
    let _lock = vm_lock();
    with_vm(|e| {
        // clHighlight = 0x8000000d -> ColorToRGB 0xff9933 (BBGGRR) -> 0x3399ff
        assert_eq!(
            e.eval("System.toActualColor(0x8000000d)", "t").unwrap(),
            TjsValue::Integer(0x3399ff)
        );
        assert_eq!(
            e.eval("System.toActualColor(0x123456)", "t").unwrap(),
            TjsValue::Integer(0x123456)
        );
        assert!(e.eval("System.toActualColor()", "t").is_err());
    });
}

#[test]
fn create_uuid_has_rfc4122_shape_and_is_unique() {
    let _lock = vm_lock();
    with_vm(|e| {
        let first = match e.eval("System.createUUID()", "t").unwrap() {
            TjsValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(first.len(), 36, "uuid: {first}");
        for (i, b) in first.as_bytes().iter().enumerate() {
            match i {
                8 | 13 | 18 | 23 => assert_eq!(*b, b'-', "uuid: {first}"),
                _ => assert!(b.is_ascii_hexdigit(), "uuid: {first}"),
            }
        }
        assert_eq!(&first[14..15], "4", "version nibble: {first}");
        assert!("89ab".contains(&first[19..20]), "variant nibble: {first}");
        let second = match e.eval("System.createUUID()", "t").unwrap() {
            TjsValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        assert_ne!(first, second);
        assert!(e.eval("System.createUUID(1)", "t").is_err());
    });
}

#[test]
fn input_string_returns_initial_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("System.inputString('Caption', 'Prompt', 'initial')", "t")
                .unwrap(),
            TjsValue::String("initial".into())
        );
        assert!(
            e.eval("System.inputString('Caption', 'Prompt')", "t")
                .is_err()
        );
    });
}

#[test]
fn inform_returns_void_or_the_first_button_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("System.inform('hello')", "t").unwrap(),
            TjsValue::Void
        );
        assert_eq!(
            e.eval("System.inform('hello', 'caption', 1)", "t").unwrap(),
            TjsValue::Integer(0)
        );
        assert!(e.eval("System.inform()", "t").is_err());
    });
}

#[test]
fn create_app_lock_reports_first_instance() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("System.createAppLock('lock')", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert!(e.eval("System.createAppLock()", "t").is_err());
    });
}

#[test]
fn assign_message_returns_true_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("System.assignMessage('id', 'hello')", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert!(e.eval("System.assignMessage('id')", "t").is_err());
    });
}

#[test]
fn properties_round_trip() {
    let _lock = vm_lock();
    with_vm(|e| {
        // title: empty state reads back the reference default.
        assert_eq!(
            e.eval("System.title", "t").unwrap(),
            TjsValue::String("krkr-rs".into())
        );
        e.exec_script("System.title = 'My Game';", "t").unwrap();
        assert_eq!(
            e.eval("System.title", "t").unwrap(),
            TjsValue::String("My Game".into())
        );
        // writable flags and integers
        assert_eq!(
            e.eval("System.eventDisabled", "t").unwrap(),
            TjsValue::Integer(0)
        );
        e.exec_script(
            "System.eventDisabled = true; System.graphicCacheLimit = 64; \
             System.drawThreadNum = 4; System.exitOnNoWindowStartup = true;",
            "t",
        )
        .unwrap();
        assert_eq!(
            e.eval("System.eventDisabled", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("System.graphicCacheLimit", "t").unwrap(),
            TjsValue::Integer(64)
        );
        assert_eq!(
            e.eval("System.drawThreadNum", "t").unwrap(),
            TjsValue::Integer(4)
        );
        assert_eq!(
            e.eval("System.exitOnNoWindowStartup", "t").unwrap(),
            TjsValue::Integer(1)
        );
        // exitOnWindowClose defaults to true in the reference.
        assert_eq!(
            e.eval("System.exitOnWindowClose", "t").unwrap(),
            TjsValue::Integer(1)
        );
        e.exec_script("System.exitOnWindowClose = false;", "t")
            .unwrap();
        assert_eq!(
            e.eval("System.exitOnWindowClose", "t").unwrap(),
            TjsValue::Integer(0)
        );
    });
}

#[test]
fn version_and_environment_properties() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert!(matches!(
            e.eval("System.versionString", "t").unwrap(),
            TjsValue::String(s) if !s.is_empty()
        ));
        assert!(matches!(
            e.eval("System.versionInformation", "t").unwrap(),
            TjsValue::String(s) if s.contains("krkr-rs")
        ));
        assert!(matches!(
            e.eval("System.platformName", "t").unwrap(),
            TjsValue::String(_)
        ));
        assert_eq!(
            e.eval("System.osName === System.platformName", "t")
                .unwrap(),
            TjsValue::Integer(1)
        );
        match e.eval("System.processorNum", "t").unwrap() {
            TjsValue::Integer(n) => assert!(n >= 1),
            other => panic!("processorNum: {other:?}"),
        }
        assert_eq!(
            e.eval("System.exeBits", "t").unwrap(),
            TjsValue::Integer((std::mem::size_of::<usize>() * 8) as i64)
        );
        assert_eq!(
            e.eval("System.osBits", "t").unwrap(),
            TjsValue::Integer((std::mem::size_of::<usize>() * 8) as i64)
        );
        assert_eq!(
            e.eval("System.exeName", "t").unwrap(),
            TjsValue::String("krkr-rs".into())
        );
    });
}

#[test]
fn storage_backed_paths_are_typed_errors_without_storage() {
    let _lock = vm_lock();
    with_vm(|e| {
        // D11: the getter needs the attached L2 storage; without it the read
        // is a typed error, never a placeholder path.
        assert!(e.eval("System.exePath", "t").is_err());
        assert!(e.eval("System.dataPath", "t").is_err());
        assert!(e.eval("System.personalPath", "t").is_err());
        assert!(e.eval("System.savedGamesPath", "t").is_err());
    });
}

#[test]
fn screen_size_reads_the_l2_scene() {
    let _lock = vm_lock();
    let mut scene = engine::Scene::default();
    scene.add_window("game", (1280, 720));
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    // SAFETY: `scene` is declared before `host` and therefore outlives the
    // binding installed below.
    unsafe { host.services_mut().attach_scene(&mut scene) };
    host.install(&engine).expect("install compat surface");

    assert_eq!(
        engine.eval("System.screenWidth", "t").unwrap(),
        TjsValue::Integer(1280)
    );
    assert_eq!(
        engine.eval("System.screenHeight", "t").unwrap(),
        TjsValue::Integer(720)
    );
}

#[test]
fn read_reg_value_without_storage_is_void_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("System.readRegValue('Version/App')", "t").unwrap(),
            TjsValue::Void
        );
        assert!(e.eval("System.readRegValue()", "t").is_err());
    });
}

#[test]
fn noop_members_do_not_throw() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script(
            "System.dumpHeap(); System.nullpo(); System.showVersion(); \
             System.doCompact(); System.doCompact(5); \
             System.touchImages([]); System.touchImages([], 100, 5);",
            "t",
        )
        .unwrap();
        assert!(e.eval("System.touchImages()", "t").is_err());
    });
}

#[test]
fn continuous_handlers_accept_dedup_and_remove() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script(
            "function h(t) {} \
             System.addContinuousHandler(h); \
             System.addContinuousHandler(h); \
             System.removeContinuousHandler(h);",
            "t",
        )
        .unwrap();
        assert!(e.eval("System.addContinuousHandler()", "t").is_err());
        assert!(e.eval("System.removeContinuousHandler()", "t").is_err());
    });
}

#[test]
fn exit_and_terminate_record_without_throwing() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(e.eval("System.exit(3)", "t").unwrap(), TjsValue::Void);
        assert_eq!(e.eval("System.terminate(0)", "t").unwrap(), TjsValue::Void);
        // terminate takes an optional code
        assert_eq!(e.eval("System.terminate()", "t").unwrap(), TjsValue::Void);
    });
}

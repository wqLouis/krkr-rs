//! Integration tests for the `System` native class and the TVP global
//! constants.
//!
//! Every test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`], because the engine → context binding is
//! process-wide.

mod common;

use api::host::TouchPhase;
use common::{Env, vm_lock};
use compat::{CompatHost, EventAdapter};
use platform::{InputEvent, PhysicalKey, PlatformEvent};
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

/// Build a VM + host and hand both to `f`, with no L2 input attached.
fn with_host(f: impl FnOnce(&Tjs2Engine, &CompatHost)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install compat surface");
    f(&engine, &host);
}

/// Build a VM + host with the L2 scene attached.
///
/// `scene` is declared before `host`, so `host` (and its binding) drops first
/// while the scene state is still alive.
fn with_scene(f: impl FnOnce(&Tjs2Engine)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut scene = engine::Scene::default();
    let mut host = CompatHost::new();
    // SAFETY: `scene` outlives the host binding.
    unsafe { host.services_mut().attach_scene(&mut scene) };
    host.install(&engine).expect("install compat surface");
    f(&engine);
}

/// Build a VM + host with the L2 input state attached.
///
/// `input` is declared before `host`, so `host` (and its binding) drops first
/// while the input state is still alive.
fn with_input(f: impl FnOnce(&Tjs2Engine, &mut CompatHost)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut input = Box::new(engine::input::InputState::new());
    let mut host = CompatHost::new();
    // SAFETY: `input` outlives the host binding.
    unsafe {
        host.services_mut()
            .attach_input(input.as_mut() as *mut engine::input::InputState);
    }
    host.install(&engine).expect("install compat surface");
    f(&engine, &mut host);
}

/// Feed one frame of platform events through the input adapter seam.
fn input_frame(
    adapter: &mut EventAdapter,
    host: &mut CompatHost,
    events: impl IntoIterator<Item = PlatformEvent>,
) {
    let mut ctx = host.ctx().expect("bound context");
    adapter.begin_input_frame(&mut ctx);
    for event in events {
        adapter.apply_input_event(&mut ctx, &event);
    }
    adapter.end_input_frame(&mut ctx);
}

/// Evaluate `expression` and return its integer result.
fn eval_int(engine: &Tjs2Engine, expression: &str) -> i64 {
    match engine.eval(expression, "t").expect("eval succeeds") {
        TjsValue::Integer(value) => value,
        other => panic!("expected Integer for {expression:?}, got {other:?}"),
    }
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
        // A strict prefix of a stored name does not match — and the
        // reference has **no default parameter**: the second argument is
        // ignored and a miss clears the result
        // (`reference/cpp/core/base/impl/SystemImpl.cpp:709-726`).
        assert_eq!(
            e.eval("System.getArgument('-arg', 'dflt')", "t").unwrap(),
            TjsValue::Void
        );
        // `setArgument` rewrites the full-name entry.
        e.exec_script("System.setArgument('-argcheck', 'y');", "t")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-argcheck')", "t").unwrap(),
            TjsValue::String("y".into())
        );
        // A hit still returns the stored value when an extra argument tags
        // along (`reference/cpp/core/base/impl/SystemImpl.cpp:715-722`).
        assert_eq!(
            e.eval("System.getArgument('-argcheck', 7)", "t").unwrap(),
            TjsValue::String("y".into())
        );
        // A miss returns void — with or without a would-be default.
        assert_eq!(
            e.eval("System.getArgument('-missing')", "t").unwrap(),
            TjsValue::Void
        );
        assert_eq!(
            e.eval("System.getArgument('-missing', 7)", "t").unwrap(),
            TjsValue::Void
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

/// Reference: `createUUID` has no `numparams` check — an argument is
/// simply ignored and another RFC-4122 v4 string comes back
/// (`reference/cpp/core/base/SystemIntf.cpp:259-286`).
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
        // The reference ignores the extra argument instead of raising
        // (`reference/cpp/core/base/SystemIntf.cpp:259-286`).
        let third = match e.eval("System.createUUID(1)", "t").unwrap() {
            TjsValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(third.len(), 36, "uuid: {third}");
        assert_ne!(third, first);
        assert_ne!(third, second);
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
fn inform_returns_void_or_the_button_index_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        // No buttons argument clears the result (`SystemImpl.cpp:610-614`).
        assert_eq!(
            e.eval("System.inform('hello')", "t").unwrap(),
            TjsValue::Void
        );
        // The reference's dialog returns the first button (0) for one or two
        // buttons (`environ/linux/Platform.cpp:140-183`); the headless port
        // emulates the left/first button.
        for buttons in ["1", "2", "['a']", "['a', 'b']"] {
            assert_eq!(
                e.eval(
                    &format!("System.inform('hello', 'caption', {buttons})"),
                    "t"
                )
                .unwrap(),
                TjsValue::Integer(0),
                "buttons {buttons}"
            );
        }
        // No button (or more than the dialog renders) yields -1.
        for buttons in ["0", "3", "[]"] {
            assert_eq!(
                e.eval(
                    &format!("System.inform('hello', 'caption', {buttons})"),
                    "t"
                )
                .unwrap(),
                TjsValue::Integer(-1),
                "buttons {buttons}"
            );
        }
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
fn assign_message_returns_the_real_success_flag_and_checks_arity() {
    let _lock = vm_lock();
    with_vm(|e| {
        // A registered engine message is overridden and reported as success
        // (`SystemIntf.cpp:288-304`, `tjsMessage.cpp:36-43,116-120`).
        assert_eq!(
            e.eval("System.assignMessage('TJSInternalError', 'hello')", "t")
                .unwrap(),
            TjsValue::Integer(1)
        );
        // An unregistered name returns false instead of being stored
        // (`tjsMessage.cpp:116-120`).
        assert_eq!(
            e.eval("System.assignMessage('not-a-real-message', 'x')", "t")
                .unwrap(),
            TjsValue::Integer(0)
        );
        assert!(e.eval("System.assignMessage('id')", "t").is_err());
    });
}

#[test]
fn properties_round_trip() {
    let _lock = vm_lock();
    with_scene(|e| {
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
        // `System.exitOnNoWindowStartup` defaults to true
        // (`SysInitImpl.cpp:383`).
        assert_eq!(
            e.eval("System.exitOnNoWindowStartup", "t").unwrap(),
            TjsValue::Integer(1)
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
        // TVPGetVersionString() -> "major.minor.release.build"; the emulated
        // KiriKiri version is 2.32.2.426 (`reference/cpp/core/base/MsgIntf.cpp:173-177`,
        // `reference/cpp/core/base/impl/MsgImpl.cpp:23-27`).
        assert_eq!(
            e.eval("System.versionString", "t").unwrap(),
            TjsValue::String("2.32.2.426".into())
        );
        assert!(matches!(
            e.eval("System.versionInformation", "t").unwrap(),
            TjsValue::String(s) if s.starts_with("Kirikiri 2 Executable core /2.32.2.426")
                && s.contains("TJS2/")
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
        assert!(e.eval("System.exeName", "t").is_err());
    });
}

#[test]
fn exe_name_is_the_project_directory_with_a_trailing_separator() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let dir = env.game_dir().expect("game dir");
    // TVPNormalizeStorageName(ExePath()), where ExePath() is the project
    // directory (`reference/cpp/core/base/impl/SystemImpl.cpp:838-847`,
    // `reference/cpp/core/environ/Application.cpp:92`).
    assert_eq!(
        env.eval("System.exeName").unwrap(),
        TjsValue::String(format!("{}/", dir.display()))
    );
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
             System.touchImages([]); System.touchImages([], 100, 5); \
             System.clearGraphicCache();",
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

#[test]
fn host_fact_members_raise_typed_errors_when_unset() {
    let _lock = vm_lock();
    with_host(|e, _host| {
        for expression in [
            "System.appDataPath",
            "System.desktopLeft",
            "System.desktopTop",
            "System.desktopWidth",
            "System.desktopHeight",
        ] {
            let error = e
                .eval(expression, "t")
                .expect_err("the unset host fact must raise")
                .to_string();
            assert!(
                error.contains("host"),
                "{expression:?} should name the missing host seam, got: {error}"
            );
        }
        // `shellExecute` and `system` are inert in the reference and never
        // consult a host seam (`SystemImpl.cpp:77-88,674-691`).
        assert_eq!(
            e.eval("System.shellExecute('target')", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("System.system('target')", "t").unwrap(),
            TjsValue::Integer(0)
        );
        // Arity is still checked.
        assert!(e.eval("System.shellExecute()", "t").is_err());
        assert!(e.eval("System.system()", "t").is_err());
    });
}

#[test]
fn host_facts_seam_serves_the_system_members() {
    let _lock = vm_lock();
    with_host(|e, host| {
        host.set_app_data_path(|| Ok("/home/user/.local/share/game/".to_string()));
        host.set_desktop_rect(|| Ok((-1920, 0, 3840, 1080)));

        assert_eq!(
            e.eval("System.appDataPath", "t").unwrap(),
            TjsValue::String("/home/user/.local/share/game/".into())
        );
        assert_eq!(eval_int(e, "System.desktopLeft"), -1920);
        assert_eq!(eval_int(e, "System.desktopTop"), 0);
        assert_eq!(eval_int(e, "System.desktopWidth"), 3840);
        assert_eq!(eval_int(e, "System.desktopHeight"), 1080);
    });
}

#[test]
fn get_key_state_reads_the_l2_input_state() {
    let _lock = vm_lock();
    with_input(|e, host| {
        let mut adapter = EventAdapter::new();
        input_frame(
            &mut adapter,
            host,
            [PlatformEvent::Input(InputEvent::Key {
                code: PhysicalKey::Enter, // VK_RETURN
                pressed: true,
            })],
        );
        // The "pressed since the previous query" edge fires exactly once.
        assert_eq!(eval_int(e, "System.getKeyState(Key.kReturn, false)"), 1);
        assert_eq!(eval_int(e, "System.getKeyState(Key.kReturn, false)"), 0);
        // The current-state query reports the held key.
        assert_eq!(eval_int(e, "System.getKeyState(Key.kReturn)"), 1);

        input_frame(
            &mut adapter,
            host,
            [PlatformEvent::Input(InputEvent::Key {
                code: PhysicalKey::Enter, // VK_RETURN
                pressed: false,
            })],
        );
        assert_eq!(eval_int(e, "System.getKeyState(Key.kReturn)"), 0);
        assert!(e.eval("System.getKeyState()", "t").is_err());
    });
}

#[test]
fn get_key_state_without_input_raises_a_typed_error() {
    let _lock = vm_lock();
    with_vm(|e| {
        let error = e
            .eval("System.getKeyState(Key.kReturn)", "t")
            .expect_err("missing input state must raise")
            .to_string();
        assert!(error.contains("input state"), "unexpected error: {error}");
    });
}

#[test]
fn touch_device_is_the_constant_mouse_flag() {
    let _lock = vm_lock();
    // The reference `TVPGetSupportTouchDevice` returns `tdMouse | tdMouseWheel`
    // unconditionally (`SystemImpl.cpp:439-466`).
    with_vm(|e| {
        assert_eq!(eval_int(e, "System.touchDevice"), 0x300);
    });
    // An active touch contact does not change the value.
    with_input(|e, host| {
        assert_eq!(eval_int(e, "System.touchDevice"), 0x300);
        let mut adapter = EventAdapter::new();
        input_frame(
            &mut adapter,
            host,
            [PlatformEvent::Input(InputEvent::Touch {
                id: 1,
                phase: TouchPhase::Started,
                x: 10.0,
                y: 20.0,
            })],
        );
        assert_eq!(eval_int(e, "System.touchDevice"), 0x300);
    });
}

#[test]
fn clear_graphic_cache_releases_unused_bitmaps() {
    let _lock = vm_lock();
    let mut scene = engine::Scene::default();
    let bitmap = scene.add_bitmap(2, 2, vec![0u8; 16]);
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    // SAFETY: `scene` outlives the binding installed below.
    unsafe { host.services_mut().attach_scene(&mut scene) };
    host.install(&engine).expect("install compat surface");

    assert!(scene.bitmap(bitmap).is_some());
    assert_eq!(
        engine.eval("System.clearGraphicCache()", "t").unwrap(),
        TjsValue::Void
    );
    assert!(scene.bitmap(bitmap).is_none());
}

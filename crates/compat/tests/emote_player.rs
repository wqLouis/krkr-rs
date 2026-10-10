//! Reference-behaviour tests for the `emote_player` loader shim.
//!
//! `emoteplayer.dll` (`reference/cpp/plugins/EmotePlayer.cpp:1-10`) does
//! exactly one thing — `ncbAutoRegister::LoadModule(TJS_W("motionplayer.dll"))`
//! (`EmotePlayer.cpp:5-8`) — and registers no classes itself: the file has no
//! `NCB_REGISTER_CLASS`, no `NCB_ATTACH_CLASS` and no `TVPExecuteScript`.
//! The whole `Motion` namespace is owned by the sibling `motionplayer.dll`,
//! which in krkr-rs is the `motion_player` plugin the compat aggregator
//! installs right after `emote_player::register`. These tests check the
//! end-to-end loader story with citations: no surface of its own, the link
//! resolves to the built-in (no null object), and the reference
//! `Motion.EmotePlayer` entry point is reachable through the loaded
//! dependency.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + scene + host-backed storage.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/scene/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _storage: Box<Storage>,
}

impl Env {
    fn new() -> Env {
        let dir = std::env::temp_dir();
        let fs = FileSystem::host(&dir);
        let storage = Storage::mount_archives(fs, dir.clone(), std::iter::empty::<&str>())
            .expect("mount storage");
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage outlive the binding, which
        // `_host` removes on drop.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "emote-player-test")
            .map_err(|error| error.to_string())
    }

    fn int(&self, expression: &str) -> i64 {
        match self
            .eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
        {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "emote-player-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }
}

/// Reference: `emoteplayer.dll`'s whole module body is the dependency load
/// (`reference/cpp/plugins/EmotePlayer.cpp:5-8`); the file registers no
/// class, no member and no script (`EmotePlayer.cpp:1-10` contains no
/// `NCB_REGISTER_CLASS`/`NCB_ATTACH_CLASS`/`TVPExecuteScript`). So the port's
/// no-op `emote_player::register` must contribute **no** script global of its
/// own — everything reachable afterwards comes from `motionplayer.dll`
/// (`EmotePlayer.cpp:7`), i.e. the sibling `motion_player` port.
///
/// The reference registers `EmotePlayer` only as `Motion.EmotePlayer`
/// (`reference/cpp/plugins/motionplayer/main.cpp:299`
/// `NCB_REGISTER_SUBCLASS_DELAY(EmotePlayer)` plus `main.cpp:431`
/// `NCB_SUBCLASS(EmotePlayer, EmotePlayer)`), never as a top-level class:
/// the subclass registration path (`ncbRegistSubClass::RegistEnd`,
/// `reference/cpp/core/plugin/ncbind.hpp:1963-1970`) overrides
/// `ncbRegistNativeClass::RegistEnd` and therefore never runs the
/// `global->PropSet(name, ...)` that publishes a class. `Motion` itself is
/// `NCB_REGISTER_CLASS(Motion)` (`main.cpp:427`), and the emoteplayer.dll
/// section adds `D3DEmoteModule`/`D3DEmotePlayer` as `NCB_REGISTER_CLASS`
/// top-level classes (`main.cpp:565,588`).
///
/// The port's `motion_player` registers every `Motion.*` subclass top-level
/// and then aliases it into `Motion`; that extra top-level surface is part of
/// the locked `docs/surface/runtime_surface.json` (`Point`, `Circle`,
/// `MotionRect`, `EmotePlayer`, ... are all published the same way), so it is
/// pinned by `tests/runtime_surface.rs`, not by this loader test.
#[test]
fn loader_defines_no_globals_of_its_own() {
    let _lock = vm_lock();
    let env = Env::new();

    // The module name is metadata, not surface: no `emoteplayer` global and no
    // class the loader could have invented.
    for name in ["emoteplayer", "EmotePlayerPlugin"] {
        assert_eq!(
            env.int(&format!("(typeof global.{name} !== 'undefined')")),
            0,
            "{name} must not be registered by the loader shim"
        );
    }
    // The `EmotePlayer` class the loader pulls in is namespaced under `Motion`
    // (`motionplayer/main.cpp:431`), matching the reference registration.
    assert_eq!(
        env.int("(typeof Motion.EmotePlayer !== 'undefined')"),
        1,
        "Motion.EmotePlayer comes from motionplayer.dll"
    );
}

/// Reference: loading `emoteplayer.dll` pulls in `motionplayer.dll`
/// (`reference/cpp/plugins/EmotePlayer.cpp:7`), so the `Motion` namespace is
/// present the moment the plugin is linked — and the link itself resolves to
/// a built-in (no null-plugin global under the DLL's stem). This checks the
/// end-to-end story: installing the compat surface wires the family and the
/// reference `Motion.EmotePlayer` entry point is reachable.
#[test]
fn loader_exposes_the_motionplayer_classes() {
    let _lock = vm_lock();
    let env = Env::new();

    // Linking the loader resolves to the built-in table (the aggregator
    // pre-registers it), exactly like `Plugins.link("emoteplayer.dll")` in a
    // game script.
    env.run("Plugins.link('emoteplayer.dll');");
    env.run("Plugins.link('emoteplayer.dll');");
    assert_eq!(
        env.int("Plugins.getList().length"),
        1,
        "the link must be idempotent"
    );
    // A built-in link installs no null plugin object under the stem.
    assert_eq!(
        env.int("(typeof global.emoteplayer !== 'undefined')"),
        0,
        "builtin link must not install a null global"
    );

    // `emote_player::register` is a no-op, so every class below is provided by
    // `motion_player::register` — exactly as `emoteplayer.dll` loads
    // `motionplayer.dll` in the reference.
    for name in [
        "Motion",
        "Motion.EmotePlayer",
        "Motion.Player",
        "Motion.ResourceManager",
        "Motion.SeparateLayerAdaptor",
        "Motion.D3DAdaptor",
        "Motion.SourceCache",
        "Motion.Point",
        "Motion.Circle",
        "Motion.Rect",
        "Motion.Quad",
        "Motion.LayerGetter",
        "Motion.D3DEmoteModule",
    ] {
        assert_eq!(
            env.int(&format!("(typeof {name} !== 'undefined')")),
            1,
            "{name} should be registered by motion_player"
        );
    }
}

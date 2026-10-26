//! The `Window.update` argument handling.
//!
//! The update type is read from the first argument, and only when a second
//! argument is present; a call with one argument updates the window normally.
//!
//! reference/cpp/core/visual/WindowIntf.cpp:769-778
//! reference/cpp/core/visual/WindowIntf.h:36-39
//! reference/cpp/core/visual/impl/WindowImpl.cpp:1492-1495

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerState;
use tjs::Tjs2Engine;

/// A bound VM + host + attached scene, timer registry and input state.
struct Env {
    /// Dropped first: releases every retained script object before the scene,
    /// the timer registry, the input state and the engine go away.
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; never read directly.
    _input: Box<InputState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed values are declared after `host`, so they are
        // dropped later and outlive the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            host,
            engine,
            _scene: scene,
            _timer: timer,
            _input: input,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "window-update-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// The update type of the last `Window.update` request, or -1 when the
    /// window asked for no update.
    fn last_update_type(&mut self) -> i32 {
        let requests = compat::take_update_requests(self.host.services_mut());
        requests.last().map_or(-1, |(_, update_type)| *update_type)
    }
}

#[test]
fn the_update_type_is_read_from_the_first_argument_only_with_a_second_one() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run("var win = new Window(); var empty;");

    // reference/cpp/core/visual/WindowIntf.cpp:769-778 — without a second
    // argument the update is the normal one
    // (reference/cpp/core/visual/WindowIntf.h:36-39 — `utNormal` is 0).
    env.run("win.update();");
    assert_eq!(env.last_update_type(), 0);
    env.run("win.update(1);");
    assert_eq!(env.last_update_type(), 0);

    // reference/cpp/core/visual/WindowIntf.cpp:769-778 — with a second
    // argument the type is the first one.
    env.run("win.update(1, 2);");
    assert_eq!(env.last_update_type(), 1);
    env.run("win.update(0, 2);");
    assert_eq!(env.last_update_type(), 0);

    // reference/cpp/core/visual/WindowIntf.cpp:769-778 — a void second
    // argument leaves the type alone.
    env.run("win.update(1, empty);");
    assert_eq!(env.last_update_type(), 0);
}

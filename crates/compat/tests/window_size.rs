//! The window client size a script sees, with and without a launcher-forced
//! VM size.
//!
//! `Scene::forced_window_size` is the composition root's logical size: while it
//! is set, a newly created window starts at it and every script resize is
//! accepted and ignored. Without it the script owns the client size exactly as
//! the reference does.

mod common;

use common::{Env, vm_lock};

/// The first scene window's client size.
fn client_size(env: &Env) -> (u32, u32) {
    env.scene
        .windows
        .first()
        .map(|window| window.inner_size)
        .expect("a window in the scene")
}

#[test]
fn a_forced_client_size_seeds_the_window_and_overrides_script_resizes() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.scene.forced_window_size = Some((1920, 1080));
    env.run("var win = new Window();");

    // The launcher's forced size is what a new window starts with.
    assert_eq!(client_size(&env), (1920, 1080));
    assert_eq!(env.eval_int("System.screenWidth"), 1920);
    assert_eq!(env.eval_int("System.screenHeight"), 1080);

    // reference/cpp/core/visual/WindowIntf.cpp:885-893 — `setInnerSize` stores
    // the client size on the window. With a forced size the write is accepted
    // and overridden (launcher extension), so the script reads the forced size
    // back.
    env.run("win.setInnerSize(1280, 720);");
    assert_eq!(client_size(&env), (1920, 1080));
    assert_eq!(env.eval_int("win.innerWidth"), 1920);
    assert_eq!(env.eval_int("win.innerHeight"), 1080);

    // reference/cpp/core/visual/WindowIntf.cpp:830-837 — `setSize` reaches the
    // same client-size write, so it is overridden too.
    env.run("win.setSize(1024, 768);");
    assert_eq!(client_size(&env), (1920, 1080));
    assert_eq!(env.eval_int("win.width"), 1920);
    assert_eq!(env.eval_int("win.height"), 1080);

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1699-1729 — the
    // `width`/`height`/`innerWidth`/`innerHeight` setters write the client
    // size; with a forced size they are ignored.
    env.run("win.width = 640; win.innerWidth = 800;");
    assert_eq!(client_size(&env), (1920, 1080));
    assert_eq!(env.eval_int("win.width"), 1920);
    assert_eq!(env.eval_int("win.innerWidth"), 1920);
    env.run("win.height = 480; win.innerHeight = 600;");
    assert_eq!(client_size(&env), (1920, 1080));
    assert_eq!(env.eval_int("win.height"), 1080);
    assert_eq!(env.eval_int("win.innerHeight"), 1080);
    assert_eq!(env.eval_int("System.screenWidth"), 1920);

    // reference/cpp/core/visual/WindowIntf.cpp:895-903 — `setZoom` only stores
    // the ratio; it never resizes the window.
    env.run("win.setZoom(2, 1);");
    assert_eq!(env.eval_int("win.zoomNumer"), 2);
    assert_eq!(env.eval_int("win.zoomDenom"), 1);
    assert_eq!(client_size(&env), (1920, 1080));
}

#[test]
fn without_a_forced_size_the_script_owns_the_client_size() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var win = new Window();");

    // The reference opens with a default client size.
    assert_eq!(client_size(&env), (1280, 720));
    assert_eq!(env.eval_int("System.screenWidth"), 1280);

    // reference/cpp/core/visual/WindowIntf.cpp:885-893 — `setInnerSize` stores
    // the client size, which the getters and `System.screenWidth` read back.
    env.run("win.setInnerSize(1024, 768);");
    assert_eq!(client_size(&env), (1024, 768));
    assert_eq!(env.eval_int("win.innerWidth"), 1024);
    assert_eq!(env.eval_int("win.innerHeight"), 768);
    assert_eq!(env.eval_int("System.screenWidth"), 1024);
    assert_eq!(env.eval_int("System.screenHeight"), 768);

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1699-1729 — the
    // `width`/`height` setters write one axis each.
    env.run("win.width = 640;");
    assert_eq!(client_size(&env), (640, 768));
    env.run("win.height = 480;");
    assert_eq!(client_size(&env), (640, 480));
    assert_eq!(env.eval_int("win.width"), 640);
    assert_eq!(env.eval_int("System.screenWidth"), 640);

    // reference/cpp/core/visual/WindowIntf.cpp:885-889 — fewer than two
    // arguments raise in both modes.
    assert!(
        env.eval("win.setInnerSize(1)").is_err(),
        "a one-argument setInnerSize must raise"
    );
    assert!(
        env.eval("win.setSize()").is_err(),
        "a zero-argument setSize must raise"
    );
}

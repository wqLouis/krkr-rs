//! The reference split between the window client size and the logical screen.
//!
//! `Window.width`/`innerWidth` read and write the window's client size, while
//! `System.screenWidth`/`screenHeight` read the first window's primary-layer
//! size — the layer whose MainImage is the window's screen buffer. Resizing the
//! primary layer changes the screen; resizing the window does not.
//!
//! reference/cpp/core/base/impl/SystemImpl.cpp:877-895
//! reference/cpp/core/visual/WindowIntf.cpp:885-903,1518-1534,1554-1588
//! reference/cpp/core/environ/impl/TVPWindow.h:107-120,389-406

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
fn the_window_client_size_and_the_logical_screen_are_separate() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var win = new Window();");

    // reference/cpp/core/visual/WindowIntf.cpp:1518-1534,1554-1588 — the
    // `width`/`height` and `innerWidth`/`innerHeight` getters read the window's
    // client size, which starts at the default 1280x720. With no primary layer
    // yet, `System.screenWidth` falls back to that client size.
    assert_eq!(client_size(&env), (1280, 720));
    assert_eq!(env.eval_int("win.width"), 1280);
    assert_eq!(env.eval_int("win.innerWidth"), 1280);
    assert_eq!(env.eval_int("win.height"), 720);
    assert_eq!(env.eval_int("win.innerHeight"), 720);
    assert_eq!(env.eval_int("System.screenWidth"), 1280);
    assert_eq!(env.eval_int("System.screenHeight"), 720);

    // reference/cpp/core/visual/WindowIntf.cpp:885-893 — `setInnerSize` stores
    // the client size on the window.
    env.run("win.setInnerSize(1024, 768);");
    assert_eq!(client_size(&env), (1024, 768));
    assert_eq!(env.eval_int("win.innerWidth"), 1024);
    assert_eq!(env.eval_int("win.innerHeight"), 768);

    // reference/cpp/core/visual/WindowIntf.cpp:830-837 — `setSize` reaches the
    // same client-size write.
    env.run("win.setSize(800, 600);");
    assert_eq!(client_size(&env), (800, 600));
    assert_eq!(env.eval_int("win.width"), 800);
    assert_eq!(env.eval_int("win.height"), 600);

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1699-1729 — the
    // `width`/`height`/`innerWidth`/`innerHeight` setters write one axis of the
    // client size each.
    env.run("win.width = 640;");
    assert_eq!(client_size(&env), (640, 600));
    env.run("win.height = 480;");
    assert_eq!(client_size(&env), (640, 480));
    env.run("win.innerWidth = 720; win.innerHeight = 540;");
    assert_eq!(client_size(&env), (720, 540));
    assert_eq!(env.eval_int("win.width"), 720);
    assert_eq!(env.eval_int("win.innerWidth"), 720);
    assert_eq!(env.eval_int("win.height"), 540);
    assert_eq!(env.eval_int("win.innerHeight"), 540);

    // reference/cpp/core/visual/LayerManager.cpp:161-172 — the first layer on a
    // window becomes its primary layer and keeps its constructor rect; the game
    // sizes it to the screen it wants.
    env.run("var primary = new Layer(win, null); primary.setSize(720, 540);");
    assert_eq!(env.eval_int("primary.width"), 720);
    assert_eq!(env.eval_int("primary.height"), 540);
    assert_eq!(env.eval_int("System.screenWidth"), 720);
    assert_eq!(env.eval_int("System.screenHeight"), 540);

    // reference/cpp/core/base/impl/SystemImpl.cpp:877-895 — the logical screen
    // is the primary layer's size; a primary-layer resize changes it.
    env.run("primary.setSize(1600, 900);");
    assert_eq!(env.eval_int("System.screenWidth"), 1600);
    assert_eq!(env.eval_int("System.screenHeight"), 900);

    // reference/cpp/core/visual/WindowIntf.cpp:885-893 — a window client-size
    // write moves only the window, so the screen keeps the primary-layer size.
    env.run("win.setInnerSize(1024, 768);");
    assert_eq!(client_size(&env), (1024, 768));
    assert_eq!(env.eval_int("win.innerWidth"), 1024);
    assert_eq!(env.eval_int("win.innerHeight"), 768);
    assert_eq!(env.eval_int("System.screenWidth"), 1600);
    assert_eq!(env.eval_int("System.screenHeight"), 900);

    // reference/cpp/core/visual/WindowIntf.cpp:885-889 — fewer than two
    // arguments raise.
    assert!(
        env.eval("win.setInnerSize(1)").is_err(),
        "a one-argument setInnerSize must raise"
    );
    assert!(
        env.eval("win.setSize()").is_err(),
        "a zero-argument setSize must raise"
    );
}

#[test]
fn the_screen_follows_the_primary_layer_at_the_real_game_sizes() {
    let _lock = vm_lock();
    let env = Env::new();
    // The reference game path: a default window, a primary layer the game sizes
    // to its 1280x720 stage, then a larger client area for the OS window and a
    // paint zoom.
    env.run(
        "var win = new Window(); \
         var primary = new Layer(win, null); \
         primary.setSize(1280, 720); \
         win.setInnerSize(1920, 1080);",
    );

    // reference/cpp/core/base/impl/SystemImpl.cpp:877-895 — the screen is the
    // primary layer, not the client area.
    assert_eq!(env.eval_int("System.screenWidth"), 1280);
    assert_eq!(env.eval_int("System.screenHeight"), 720);
    // reference/cpp/core/visual/WindowIntf.cpp:885-893,1518-1534 — the client
    // size is what `innerWidth` reports.
    assert_eq!(env.eval_int("win.innerWidth"), 1920);
    assert_eq!(env.eval_int("win.innerHeight"), 1080);
    assert_eq!(env.eval_int("win.width"), 1920);
    assert_eq!(env.eval_int("win.height"), 1080);
    assert_eq!(client_size(&env), (1920, 1080));

    // reference/cpp/core/visual/WindowIntf.cpp:895-903 — `setZoom` only stores
    // the ratio; it never resizes the window or the screen.
    env.run("win.setZoom(150, 100);");
    assert_eq!(env.eval_int("win.zoomNumer"), 150);
    assert_eq!(env.eval_int("win.zoomDenom"), 100);
    // reference/cpp/core/environ/impl/TVPWindow.h:389-406 — SetZoom stores the
    // ratio and leaves the content size alone.
    assert_eq!(env.eval_int("win.innerWidth"), 1920);
    assert_eq!(env.eval_int("System.screenWidth"), 1280);
    assert_eq!(env.eval_int("primary.width"), 1280);
    assert_eq!(env.eval_int("primary.height"), 720);
    assert_eq!(client_size(&env), (1920, 1080));
}

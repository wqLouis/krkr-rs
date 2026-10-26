//! The `Window` members whose behaviour the window form defines: the zoom
//! fraction and the mask region, and the full-screen guard that rejects a
//! geometry change.
//!
//! The zoom is stored in lowest terms, the mask region needs a primary layer
//! (the window raises without one), and a full-screen window refuses to be
//! resized, moved or restyled.
//!
//! reference/cpp/core/environ/impl/TVPWindow.h:91-94,393-408
//! reference/cpp/core/visual/impl/WindowImpl.cpp:1183-1188,1517-1735,1803-1820
//! reference/cpp/core/visual/WindowIntf.cpp:788-805
//! reference/cpp/core/base/MsgIntfInc.h:94
//! reference/cpp/core/base/impl/MsgImpl.h:49-50

mod common;

use common::{Env, vm_lock};

#[test]
fn the_zoom_fraction_is_kept_in_lowest_terms() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var win = new Window();");

    // reference/cpp/core/environ/impl/TVPWindow.h:393-408 — the pair is
    // divided by its common divisor, so 150/100 is kept as 3/2.
    env.run("win.setZoom(150, 100);");
    assert_eq!(env.eval_int("win.zoomNumer"), 3);
    assert_eq!(env.eval_int("win.zoomDenom"), 2);

    // reference/cpp/core/environ/impl/TVPWindow.h:393-408 — a coprime pair
    // is kept as it is.
    env.run("win.setZoom(2, 3);");
    assert_eq!(env.eval_int("win.zoomNumer"), 2);
    assert_eq!(env.eval_int("win.zoomDenom"), 3);

    // reference/cpp/core/environ/impl/TVPWindow.h:393-408 — 4/8 is 1/2.
    env.run("win.setZoom(4, 8);");
    assert_eq!(env.eval_int("win.zoomNumer"), 1);
    assert_eq!(env.eval_int("win.zoomDenom"), 2);

    // reference/cpp/core/environ/impl/TVPWindow.h:91-94 — writing one half
    // of the fraction reduces the pair again.
    env.run("win.zoomNumer = 6;");
    assert_eq!(env.eval_int("win.zoomNumer"), 3);
    assert_eq!(env.eval_int("win.zoomDenom"), 1);
    env.run("win.zoomDenom = 12;");
    assert_eq!(env.eval_int("win.zoomNumer"), 1);
    assert_eq!(env.eval_int("win.zoomDenom"), 4);

    // reference/cpp/core/environ/impl/TVPWindow.h:393-408 — a zero divisor
    // has no reduction, and the pair is kept instead of dividing by zero.
    env.run("win.setZoom(0, 0);");
    assert_eq!(env.eval_int("win.zoomNumer"), 0);
    assert_eq!(env.eval_int("win.zoomDenom"), 0);
}

#[test]
fn the_mask_region_needs_a_primary_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var win = new Window();");

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1803-1814 — a window
    // without a primary layer raises;
    // reference/cpp/core/base/MsgIntfInc.h:94 — the message is
    // "Window has no layer".
    let message = env.error("win.setMaskRegion(1)");
    assert!(
        message.contains("Window has no layer"),
        "the missing layer must be named: {message}"
    );

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1803-1814 — with a
    // primary layer the call returns.
    env.run("var primary = new Layer(win, null);");
    env.run("win.setMaskRegion(1);");

    // reference/cpp/core/visual/WindowIntf.cpp:788-797 — the threshold is
    // optional.
    env.run("win.setMaskRegion();");

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1816-1820 — removing the
    // mask region is always allowed.
    env.run("win.removeMaskRegion();");
}

#[test]
fn a_full_screen_window_rejects_a_geometry_change() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var win = new Window(); win.fullScreen = true;");

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1183-1188 — the window
    // raises instead of changing a full-screen window;
    // reference/cpp/core/base/impl/MsgImpl.h:49-50 — the message is
    // "Invalid property in fullscreen : %1".
    //
    // The guarded members are the ones the reference guards in
    // `tTJSNI_Window` (WindowImpl.cpp:1517-1735): the visibility, the whole
    // size and position family, the border style and the two window methods.
    for member in [
        "win.visible = false",
        "win.width = 100",
        "win.height = 100",
        "win.innerWidth = 100",
        "win.innerHeight = 100",
        "win.left = 10",
        "win.top = 10",
        "win.minWidth = 10",
        "win.minHeight = 10",
        "win.maxWidth = 10",
        "win.maxHeight = 10",
        "win.borderStyle = 2",
        "win.innerSunken = true",
        "win.setSize(100, 100)",
        "win.setInnerSize(100, 100)",
        "win.setPos(10, 10)",
        "win.setMinSize(10, 10)",
        "win.setMaxSize(10, 10)",
        "win.beginMove()",
        "win.showModal()",
    ] {
        let message = env.error(member);
        assert!(
            message.contains("Invalid property in fullscreen"),
            "{member} must be rejected by a full-screen window: {message}"
        );
    }

    // reference/cpp/core/visual/impl/WindowImpl.cpp:1535-1540 — a window that
    // is not full-screen takes the change.
    env.run("var plain = new Window(); plain.width = 640; plain.height = 480;");
    assert_eq!(env.eval_int("plain.width"), 640);
    assert_eq!(env.eval_int("plain.height"), 480);
}

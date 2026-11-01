//! Integration tests for the province plane: the 8-bit index plane a layer
//! reads through `getProvincePixel`.
//!
//! The plane is the image's own 8-bit sample — the palette index of a
//! palettized image — and not the colour the palette maps that index to
//! (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709,1838`,
//! `reference/cpp/core/visual/LoadPNG.cpp:278-290`). A map region is
//! therefore addressed by its index, and a region whose index the action
//! table does not know is not a region at all.
//!
//! The tests install the real L1 `media` decoder through the host's codec
//! seam, so the committed fixture is decoded by the code the game runs. The
//! environment is the shared harness's: it mounts the storage, runs the
//! script and delivers the events.

mod common;

use common::{Env, vm_lock};
use media::{Media, decode_index_plane};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// The committed fixture: a 4x4 palettized PNG whose palette index is the
/// region id.
///
/// The palette is decorative — index 16 is the red (235, 0, 0), index 17 the
/// darker (215, 0, 0) — so a plane that carried the colour instead of the
/// index would answer 235 where the region id is 16. The first row is outside
/// every region (index 0).
const FIXTURE: &[u8] = include_bytes!("fixtures/province.png");

/// The fixture's name in the mounted storage.
const FIXTURE_NAME: &str = "province.png";

/// A bound environment with the fixture mounted and the real L1 index-plane
/// decoder installed.
fn province_env() -> Env {
    let env = Env::with_storage();
    let dir = env.game_dir().expect("the temp game directory");
    std::fs::write(dir.join(FIXTURE_NAME), FIXTURE).expect("write the province image");
    let media = Media::new();
    env.install_index_plane_decoder(move |bytes, hint| {
        decode_index_plane(&media, bytes, hint).map_err(|error| error.to_string())
    });
    env
}

/// The province plane holds the palette index of the image, not the colour
/// the palette maps it to.
///
/// Reference: the province image is loaded as a palettized bitmap whose
/// plane keeps the palette indexes
/// (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709,1838`), and the PNG
/// loader leaves the 8-bit plane alone
/// (`reference/cpp/core/visual/LoadPNG.cpp:278-290,1092-1116`). The fixture's
/// index 16 is the decorative red (235, 0, 0), so a plane that carried the
/// colour would answer 235 where the region id is 16.
#[test]
fn the_province_plane_holds_the_palette_index() {
    let _lock = vm_lock();
    let env = province_env();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4); \
         l.loadProvinceImage('province.png');",
    );

    assert_eq!(
        env.eval_int("l.getProvincePixel(0, 1)"),
        16,
        "the plane holds the palette index, not the palette colour"
    );
    assert_eq!(env.eval_int("l.getProvincePixel(2, 1)"), 17);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 2)"), 18);
    assert_eq!(env.eval_int("l.getProvincePixel(2, 3)"), 21);
    assert_eq!(
        env.eval_int("l.getProvincePixel(0, 0)"),
        0,
        "the row outside every region stays 0"
    );
}

/// A region is addressed by its index, so the action table of a map image
/// finds the region a click landed on.
///
/// The shape is the one a KAG map layer uses: the click is turned into the
/// point's province value, and the value selects the action
/// (`reference/cpp/core/visual/LayerIntf.cpp:2973-2981` reads the plane).
/// The hit test reads the same plane, so the region outside every id is not
/// a hit at all (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914` sizes
/// the plane, and the province hit test takes a non-zero plane byte).
///
/// The table holds the region's **action id** (a number), which is what a
/// truthiness test reads: a TJS2 string is truthy only when it converts to a
/// non-zero number, so a named action would read as absent.
#[test]
fn a_click_on_a_region_runs_the_region_action() {
    let _lock = vm_lock();
    let env = province_env();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var map = new Layer(w, null); \
         map.setPos(0, 0); map.setSize(4, 4); map.visible = true; map.focusable = true; \
         map.setImageSize(4, 4); \
         map.hitType = 1; \
         map.loadProvinceImage('province.png'); \
         var actions = %[16 => 1]; \
         var fired = 0; \
         function processProvince(x, y) { \
             var region = map.getProvincePixel(x, y); \
             if (region == 0) return; \
             var action = actions[region]; \
             if (action) { fired = fired + 1; } \
         } \
         map.onMouseDown = function(x, y, button, shift) { processProvince(x, y); };",
    );

    // The press inside the region 16 (the second row, first column).
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 0.0, y: 1.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(
        env.eval_int("fired"),
        1,
        "the click on region 16 runs the region's action"
    );

    // The press outside every region: the plane byte is 0, so the point is
    // not a hit and no action runs.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 0.0, y: 0.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(
        env.eval_int("fired"),
        1,
        "the click outside every region runs no action"
    );
}

/// A region the action table does not know is not a region: the plane value
/// is looked up as it is, so a colour-derived value would find nothing.
///
/// Reference: the province value is the palette index
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709`), and the map
/// layer's action table is keyed by that index.
#[test]
fn a_region_the_action_table_does_not_know_runs_no_action() {
    let _lock = vm_lock();
    let env = province_env();
    env.run(
        "var w = new Window(); \
         var map = new Layer(w, null); \
         map.setImageSize(4, 4); \
         map.loadProvinceImage('province.png'); \
         var actions = %[16 => 1]; \
         var fired = 0; \
         function processProvince(x, y) { \
             var region = map.getProvincePixel(x, y); \
             if (region == 0) return; \
             var action = actions[region]; \
             if (action) { fired = fired + 1; } \
         } \
         processProvince(0, 1); \
         processProvince(2, 1);",
    );

    assert_eq!(
        env.eval_int("fired"),
        1,
        "only the region the table knows runs an action"
    );
}

/// A province image that cannot be read raises instead of answering a
/// colour.
///
/// Reference: the province load reports the storage failure unchanged
/// (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`), and the plane is
/// dropped when the load fails.
#[test]
fn a_missing_province_image_raises() {
    let _lock = vm_lock();
    let env = province_env();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4);",
    );

    let message = env.error("l.loadProvinceImage('missing.png');");
    assert!(
        message.contains("not found"),
        "the missing province image raises: {message}"
    );
}

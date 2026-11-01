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
use engine::{StorageMedia, StorageReadError};
use media::{Media, decode_image, decode_index_plane, probe_image};
use platform::{InputEvent, MouseButton, PlatformEvent};
use testkit::{encode_png, pattern};

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

/// A bound environment with the real L1 decoders installed.
fn media_env() -> Env {
    let env = Env::with_storage();
    let media = Media::new();
    env.install_image_codec(
        move |bytes, hint| decode_image(&media, bytes, hint).map_err(|error| error.to_string()),
        move |bytes, hint| probe_image(&media, bytes, hint).map_err(|error| error.to_string()),
        |_name, _type, _width, _height, _rgba| Ok(()),
    );
    env.install_index_plane_decoder(move |bytes, hint| {
        decode_index_plane(&media, bytes, hint).map_err(|error| error.to_string())
    });
    env
}

/// A bound environment with the province fixture mounted.
fn province_env() -> Env {
    let env = media_env();
    write_game_file(&env, FIXTURE_NAME, FIXTURE);
    env
}

/// Writes `bytes` into the mounted game directory under `name`.
fn write_game_file(env: &Env, name: &str, bytes: &[u8]) {
    let dir = env.game_dir().expect("the temp game directory");
    std::fs::write(dir.join(name), bytes).expect("write the game file");
}

/// A media that serves the graphic and fails on the province name.
///
/// The failure is a genuine media error and not a missing member, so the
/// province load must raise instead of quietly loading the graphic without a
/// plane.
struct FailingProvinceMedia {
    /// The bytes the media serves for every name but the province one.
    graphic: Vec<u8>,
}

impl StorageMedia for FailingProvinceMedia {
    fn domain(&self) -> &str {
        "boom"
    }

    fn exists(&self, path: &str) -> Result<bool, StorageReadError> {
        Ok(path != "pack/27_p.png")
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, StorageReadError> {
        if path == "pack/27_p.png" {
            return Err(StorageReadError::Media {
                media: "boom".to_string(),
                message: "the packed province could not be read".to_string(),
            });
        }
        Ok(self.graphic.clone())
    }
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

/// A loaded graphic takes its province image, when the storage holds it.
///
/// The province graphic of a graphic is the graphic whose name carries `_p`
/// before the extension (`map.png` → `map_p.png`), and a load takes it when
/// the storage has one (`reference/cpp/core/visual/LayerIntf.cpp:2847-2875`,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1662-1680`). The plane
/// is the index plane of that file, so it holds the region ids.
#[test]
fn a_loaded_graphic_takes_its_province_image() {
    let _lock = vm_lock();
    let env = media_env();
    // The graphic and the province image that belongs to it.
    let main = encode_png(4, 4, &pattern(4, 4));
    write_game_file(&env, "map.png", &main);
    write_game_file(&env, "map_p.png", FIXTURE);

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('map.png');",
    );

    assert_eq!(
        env.eval_int("l.getProvincePixel(0, 1)"),
        16,
        "the loaded graphic's province image is its plane"
    );
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 4);
}

/// A graphic whose province image the storage does not hold leaves the plane
/// empty — the load itself succeeds.
///
/// Reference: the province image is optional
/// (`reference/cpp/core/visual/LayerIntf.cpp:2847-2875`), so a storage
/// without `map_p.png` loads the graphic and no plane.
#[test]
fn a_graphic_without_a_province_image_loads_without_a_plane() {
    let _lock = vm_lock();
    let env = media_env();
    let main = encode_png(4, 4, &pattern(4, 4));
    write_game_file(&env, "plain.png", &main);

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('plain.png');",
    );

    assert_eq!(
        env.eval_int("l.provinceImageBufferPitch"),
        0,
        "a graphic without a province image leaves the plane empty"
    );
    assert_eq!(env.eval_int("l.getProvincePixel(0, 1)"), 0);
}

/// A province lookup that fails for a reason other than a missing member
/// fails the load.
///
/// The reference's province load reports the storage failure unchanged
/// (`reference/cpp/core/visual/LayerIntf.cpp:2847-2875`), so a media that
/// cannot read the province member raises instead of loading the graphic
/// without a plane.
#[test]
fn a_media_failure_in_the_province_lookup_raises() {
    let _lock = vm_lock();
    let env = media_env();
    let graphic = encode_png(4, 4, &pattern(4, 4));
    env.storage()
        .expect("the mounted storage")
        .register_media(Box::new(FailingProvinceMedia { graphic }));
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null);",
    );

    let message = env.error("l.loadImages('boom://pack/27.png');");
    assert!(
        message.contains("the packed province could not be read"),
        "the media failure must reach the script: {message}"
    );
}

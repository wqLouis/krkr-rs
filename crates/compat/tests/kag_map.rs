//! The KAG clickable province map.
//!
//! A map is a province image plus an action table: the image's eight-bit
//! plane says which region a point belongs to, and the table says what that
//! region does. The click is the whole chain — the point's region, the
//! region's action, the action's own effect.
//!
//! The map layer is the KAG runtime's (`system/graphiclayer.tjs` in the KAG
//! the games ship): it keeps the action table, answers `queryProvinceAction`
//! and turns a click into `processProvince`. The engine half is the port's:
//! the province plane (`Layer.loadProvinceImage`), the value a point answers
//! (`Layer.getProvincePixel`) and the click target (the hit test and the
//! event adapter).
//!
//! Reference:
//!   * `reference/cpp/core/visual/LayerIntf.cpp:2893-2918` —
//!     `LoadProvinceImage`: the province image is loaded into the layer's
//!     eight-bit plane, sized to the main image.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:2973-2982` —
//!     `GetProvincePixel`: the plane's value at a point, 0 outside it.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:3286-3302` — the hit test:
//!     the layer under the pointer is the one that receives the click.
//!   * `reference/cpp/core/visual/LayerIntf.h:113` — `htMask` / `htProvince`.
//!   * `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709` — the plane is
//!     the image's own eight-bit sample, the palette index of a palettized
//!     image.
//!   * `reference/cpp/core/base/KAGParser.cpp:1384-1400` — the tag
//!     dictionary: the tag's name and its attributes.
//!
//! The KAG runtime the map belongs to — the oracle for the tag handlers, the
//! action table's format and the map layer:
//!   * `system/mainwindow.tjs` — the `mapimage`, `mapaction` and
//!     `mapdisable` tag handlers.
//!   * `system/graphiclayer.tjs` — the map layer: the action table
//!     (`internalLoadProvinceActions`), the region's action
//!     (`queryProvinceAction`) and the click (`processProvince`).

mod common;

use common::map;
use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::TjsValue;

/// A window with the map layer in it: the province image, the action table
/// and the layer that answers a click.
fn map_env() -> Env {
    let env = Env::with_storage();
    map::install(&env, "province_map", "province_map.ma");
    env.run(map::MAP_LAYER_TJS);
    env.run(map::MAP_SETUP_TJS);
    env
}

/// The province plane holds the region's id, so the action table finds the
/// region a click landed on.
///
/// Reference: the province image is loaded into the layer's eight-bit plane
/// (`reference/cpp/core/visual/LayerIntf.cpp:2893-2918`), and that plane is
/// the image's own sample — the palette index of a palettized image
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709`). The fixture's
/// index 16 is the decorative red, so a plane that carried the colour would
/// answer 235 where the region id is 16.
#[test]
fn the_province_plane_answers_the_region_id() {
    let _lock = vm_lock();
    let env = map_env();
    env.run("map.loadProvinceImage('province_map');");

    assert_eq!(
        env.eval_int("map.getProvincePixel(1, 1)"),
        16,
        "the plane holds the region's id"
    );
    assert_eq!(env.eval_int("map.getProvincePixel(6, 6)"), 17);
    assert_eq!(
        env.eval_int("map.getProvincePixel(5, 1)"),
        0,
        "the point outside every region is no region"
    );
}

/// The action table is keyed by the region's id, and a region the table does
/// not know is not a region.
///
/// Reference: the region's value is the plane's value
/// (`reference/cpp/core/visual/LayerIntf.cpp:2973-2982`), and the map layer
/// looks the action up by that value (`system/graphiclayer.tjs`).
#[test]
fn the_action_table_is_keyed_by_the_region_id() {
    let _lock = vm_lock();
    let env = map_env();
    env.run(
        "map.loadProvinceImage('province_map'); \
         map.internalLoadProvinceActions('province_map.ma');",
    );

    assert_eq!(
        env.eval_ok("map.queryProvinceAction(16).target"),
        TjsValue::String("*game_start".into()),
        "the region's action is the table's own"
    );
    assert_eq!(
        env.eval_ok("map.queryProvinceAction(17).target"),
        TjsValue::String("*game_load".into())
    );
    assert_eq!(
        env.eval_int("map.queryProvinceAction(0) === void"),
        1,
        "the region outside every region has no action"
    );
    assert_eq!(
        env.eval_int("map.queryProvinceAction(99) === void"),
        1,
        "a region the table does not know has no action"
    );
}

/// The click target is the map layer, and a click on a region runs the
/// region's action.
///
/// Reference: the hit test picks the layer under the pointer
/// (`reference/cpp/core/visual/LayerIntf.cpp:3286-3302`), and the map layer
/// turns the click into the region's action (`system/graphiclayer.tjs`).
#[test]
fn a_click_on_a_region_runs_the_region_action() {
    let _lock = vm_lock();
    let env = map_env();
    env.run(
        "map.loadProvinceImage('province_map'); \
         map.internalLoadProvinceActions('province_map.ma');",
    );

    assert_eq!(
        env.eval_int("base.getLayerAt(1, 1) === map"),
        1,
        "the map layer is the layer under the pointer"
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 1.0, y: 1.0 }),
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
        env.eval_int("mapFired.count"),
        1,
        "the click on region 16 runs the region's action"
    );
    assert_eq!(
        env.eval_int("mapFired[0]"),
        16,
        "the action is the region's own"
    );
}

/// A click outside every region runs no action: the point is in no region,
/// so there is nothing to run.
///
/// Reference: the point's region is the plane's value
/// (`reference/cpp/core/visual/LayerIntf.cpp:2973-2982`), and the map layer
/// looks the action up by that value (`system/graphiclayer.tjs`).
#[test]
fn a_click_outside_every_region_runs_no_action() {
    let _lock = vm_lock();
    let env = map_env();
    env.run(
        "map.loadProvinceImage('province_map'); \
         map.internalLoadProvinceActions('province_map.ma');",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 5.0, y: 1.0 }),
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
        env.eval_int("mapFired.count"),
        0,
        "the click outside every region runs no action"
    );
}

/// The pointer entering a region runs the region's enter action, and leaving
/// it runs the leave action.
///
/// Reference: the map layer reads the point's region on every move and runs
/// the region's enter and leave actions (`system/graphiclayer.tjs`).
#[test]
fn the_pointer_entering_a_region_runs_the_enter_action() {
    let _lock = vm_lock();
    let env = map_env();
    env.run(
        "map.loadProvinceImage('province_map'); \
         map.internalLoadProvinceActions('province_map.ma');",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 5.0, y: 1.0 }),
        PlatformEvent::Input(InputEvent::PointerMoved { x: 1.0, y: 1.0 }),
    ]);

    assert_eq!(
        env.eval_int("mapEntered.count"),
        1,
        "the pointer entering region 16 runs the region's enter action"
    );
    assert_eq!(env.eval_int("mapEntered[0]"), 16);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 5.0,
        y: 1.0,
    })]);

    assert_eq!(
        env.eval_int("mapLeft.count"),
        1,
        "the pointer leaving region 16 runs the region's leave action"
    );
    assert_eq!(env.eval_int("mapLeft[0]"), 16);
}

/// The KAG tag layer hands the map tags to the runtime with their
/// attributes: the tags are ordinary tags, and the runtime's own handlers
/// act on them.
///
/// Reference: the tag dictionary carries the tag's name and its attributes
/// (`reference/cpp/core/base/KAGParser.cpp:1384-1400`), and the KAG runtime
/// handles the map tags in `system/mainwindow.tjs`.
#[test]
fn the_map_tags_reach_the_runtime_with_their_attributes() {
    let _lock = vm_lock();
    let env = map_env();
    env.run(
        r#"
        var scenario = [
            "[mapimage layer = base page = fore storage = \"province_map\"]",
            "[mapaction layer = base page = fore storage = \"province_map.ma\"]",
            "[mapdisable layer = base page = fore]",
            "",
        ].join("\r\n");

        class MapParser extends KAGParser
        {
            function MapParser() { super.KAGParser(); }
            function onScenarioLoad(name) { return scenario; }
        }

        var parser = new MapParser();
        parser.loadScenario("map.ks");
        var tagNames = [];
        var tagStorages = [];
        var tagLayers = [];
        var tagPages = [];
        while(true)
        {
            var tag = parser.getNextTag();
            if(tag === void) break;
            if(tag.tagname == "ch" || tag.tagname == "r") continue;
            tagNames.push(tag.tagname);
            tagStorages.push(tag.storage === void ? "" : tag.storage);
            tagLayers.push(tag.layer === void ? "" : tag.layer);
            tagPages.push(tag.page === void ? "" : tag.page);
        }
        "#,
    );

    assert_eq!(
        env.eval_int("tagNames.count"),
        3,
        "the three map tags reach the runtime"
    );
    assert_eq!(
        env.eval_ok("tagNames[0]"),
        TjsValue::String("mapimage".into())
    );
    assert_eq!(
        env.eval_ok("tagNames[1]"),
        TjsValue::String("mapaction".into())
    );
    assert_eq!(
        env.eval_ok("tagNames[2]"),
        TjsValue::String("mapdisable".into())
    );
    assert_eq!(
        env.eval_ok("tagStorages[0]"),
        TjsValue::String("province_map".into()),
        "the map image's storage is the tag's own"
    );
    assert_eq!(
        env.eval_ok("tagStorages[1]"),
        TjsValue::String("province_map.ma".into()),
        "the action table's storage is the tag's own"
    );
    assert_eq!(
        env.eval_ok("tagLayers[0]"),
        TjsValue::String("base".into()),
        "the layer the tag names is the tag's own"
    );
    assert_eq!(env.eval_ok("tagPages[0]"), TjsValue::String("fore".into()));
}

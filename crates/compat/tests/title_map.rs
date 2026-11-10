//! The title screen's clickable province map.
//!
//! The title arms a map, waits for the player to click a region and disables
//! the map again. The map is a province image — the plane that says which
//! region a point belongs to — and an action table — what each region does.
//! The title's own files are the province image `title_bg_main_p` and the
//! action table `title_bg_main.ma`.
//!
//! The test reproduces that flow: the title's map tags reach the KAG runtime,
//! the runtime's handlers arm the map layer, a click on a region runs that
//! region's action, and the `mapdisable` tag disarms the map again.
//!
//! Reference:
//!   * `reference/cpp/core/visual/LayerIntf.cpp:2893-2918` —
//!     `LoadProvinceImage`: the province image is loaded into the layer's
//!     eight-bit plane, sized to the main image.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:2973-2982` —
//!     `GetProvincePixel`: the plane's value at a point, 0 outside it.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:3286-3302` — the hit test:
//!     the layer under the pointer is the one that receives the click.
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

/// The title's map tags, in the order the title uses them: the province
/// image, the action table, and the tag that disarms the map again.
const TITLE_TAGS: &str = r#"
        var scenario = [
            "[mapimage layer = base page = fore storage = \"title_bg_main_p\"]",
            "[mapaction layer = base page = fore storage = \"title_bg_main.ma\"]",
            "[mapdisable layer = base page = fore]",
            "",
        ].join("\r\n");

        class TitleParser extends KAGParser
        {
            function TitleParser() { super.KAGParser(); }
            function onScenarioLoad(name) { return scenario; }
        }

        // The KAG runtime's tag handlers (`system/mainwindow.tjs`): the map
        // tags reach the layer they name.
        function applyTag(tag)
        {
            if(tag.tagname == "mapimage") map.loadProvinceImage(tag.storage);
            else if(tag.tagname == "mapaction") map.internalLoadProvinceActions(tag.storage);
            else if(tag.tagname == "mapdisable") map.clearProvinceActions();
        }

        // The tags the title's scenario holds, in order.
        //
        // The reference hands the same tag dictionary to every tag
        // (`reference/cpp/core/base/KAGParser.cpp:329`, `:1391`), so the
        // runtime copies the members it keeps instead of keeping the
        // dictionary itself.
        var titleTags = [];
        var parser = new TitleParser();
        parser.loadScenario("title.ks");
        while(true)
        {
            var tag = parser.getNextTag();
            if(tag === void) break;
            if(tag.tagname == "ch" || tag.tagname == "r") continue;
            var kept = %[];
            kept.tagname = tag.tagname;
            kept.storage = tag.storage;
            kept.layer = tag.layer;
            kept.page = tag.page;
            titleTags.push(kept);
        }
"#;

/// A window with the map layer in it, the title's files in the storage and
/// the title's tags read.
fn title_env() -> Env {
    let env = Env::with_storage();
    map::install(&env, "title_bg_main_p", "title_bg_main.ma");
    env.run(map::MAP_LAYER_TJS);
    env.run(map::MAP_SETUP_TJS);
    env.run(TITLE_TAGS);
    env
}

/// The title's map tags reach the runtime with the title's own storages.
#[test]
fn the_title_map_tags_name_the_title_files() {
    let _lock = vm_lock();
    let env = title_env();

    assert_eq!(
        env.eval_int("titleTags.count"),
        3,
        "the title's three map tags reach the runtime"
    );
    assert_eq!(
        env.eval_ok("titleTags[0].tagname"),
        tjs::TjsValue::String("mapimage".into())
    );
    assert_eq!(
        env.eval_ok("titleTags[0].storage"),
        tjs::TjsValue::String("title_bg_main_p".into()),
        "the province image is the title's own"
    );
    assert_eq!(
        env.eval_ok("titleTags[1].storage"),
        tjs::TjsValue::String("title_bg_main.ma".into()),
        "the action table is the title's own"
    );
    assert_eq!(
        env.eval_ok("titleTags[0].layer"),
        tjs::TjsValue::String("base".into()),
        "the map is the title's background layer"
    );
}

/// The title's map: the tags arm the map layer, and a click on a region runs
/// that region's action.
///
/// Reference: the hit test picks the layer under the pointer
/// (`reference/cpp/core/visual/LayerIntf.cpp:3286-3302`), the region's value
/// is the plane's (`reference/cpp/core/visual/LayerIntf.cpp:2973-2982`), and
/// the map layer turns the click into the region's action
/// (`system/graphiclayer.tjs`).
#[test]
fn a_click_on_the_title_map_runs_the_regions_action() {
    let _lock = vm_lock();
    let env = title_env();

    // The title's first two tags arm the map.
    env.run("applyTag(titleTags[0]); applyTag(titleTags[1]);");

    assert_eq!(
        env.eval_int("map.getProvincePixel(1, 1)"),
        16,
        "the title's province image is the map's plane"
    );
    assert_eq!(
        env.eval_ok("map.queryProvinceAction(16).target"),
        tjs::TjsValue::String("*game_start".into()),
        "the title's action table is the map's table"
    );

    // The click on the region.
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
        "the click on the region runs the region's action"
    );
    assert_eq!(
        env.eval_int("mapFired[0]"),
        16,
        "the action is the region's own"
    );

    // The title's last tag disarms the map: the click runs no action any
    // more.
    env.run("applyTag(titleTags[2]);");
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 6.0, y: 6.0 }),
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
        "the disarmed map runs no action"
    );
}

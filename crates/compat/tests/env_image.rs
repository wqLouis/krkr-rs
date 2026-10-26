//! Behaviour tests for the environment image path.
//!
//! The game's environment layer is the layer a motion pose is painted into.
//! Two things decide whether the pose is presented: the target layer is given
//! the size of the canvas the pose is drawn into, and the timeline
//! information the motion player publishes carries the members the game's
//! scripts read.
//!
//! The first test pins the size. A layer whose rectangle was emptied keeps
//! the image it was allocated with, and the draw hands the layer the size of
//! that image back: a layer with an empty rectangle draws nothing, so a pose
//! computed into its image would never reach the screen.
//!
//! The second pins the timeline information. Every entry carries the members
//! the game's `AffineSourceMotion` reads from it, `label` among them; a
//! missing member is a fatal script error in the game.

mod common;

use common::{Env, vm_lock};

/// The motion fixture the reference ships with the emote player tests.
const MOTION_FIXTURE: &str = "e-mote3.0バニラパジャマa.psb";

/// The seed the fixture is encrypted with.
const MOTION_SEED: i64 = 742877301;

/// The index of the layer with the given name.
fn layer_index(env: &Env, name: &str) -> usize {
    env.scene
        .layers
        .iter()
        .position(|layer| layer.name == name)
        .expect("the layer exists")
}

/// The size of the target layer's rectangle.
fn layer_size(env: &Env, layer: usize) -> (u32, u32) {
    let state = env.scene.layers.get(layer).expect("the layer exists");
    (state.rect.w, state.rect.h)
}

/// The draw gives the target layer the size of the canvas it drew into.
///
/// The reference asks the target layer for the size of the canvas it is drawn
/// into — the layer's own rectangle, and its image when that rectangle is
/// empty (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:346-361`) —
/// and gives the layer that size before the pose is drawn
/// (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1126-1131`).
/// A layer with an empty rectangle draws nothing at all, so without that step
/// the pose the player computes is never presented.
#[test]
fn the_draw_gives_the_target_layer_the_canvas_size() {
    let _lock = vm_lock();
    let env = Env::new();
    // The motion player's classes exist only after its first link, the way
    // a game script links it.
    env.link_plugin("motionplayer.dll");
    env.run(
        "var w = new Window();\
         w.setSize(64, 64);\
         var base = new Layer(w, null);\
         var l = new Layer(w, base);\
         l.name = \"envtitle\";\
         l.setSize(32, 32);\
         l.hasImage = true;\
         l.setPos(0, 0, 0, 0);",
    );
    // The rectangle is empty while the image keeps the size it was allocated
    // with: emptying a rectangle never shrinks the image
    // (`reference/cpp/core/visual/LayerIntf.cpp:2664-2673`, the image size
    // only grows to cover the rectangle).
    assert_eq!(env.eval_int("l.width"), 0);
    assert_eq!(env.eval_int("l.height"), 0);
    assert_eq!(env.eval_int("l.imageWidth"), 32);
    assert_eq!(env.eval_int("l.imageHeight"), 32);

    // `draw(tTJSVariant)` takes the target layer
    // (`reference/cpp/plugins/motionplayer/main.cpp:234`).
    env.run("var p = new Motion.Player(null); p.draw(l);");

    assert_eq!(
        layer_size(&env, layer_index(&env, "envtitle")),
        (32, 32),
        "the target layer keeps the canvas size the pose was drawn into"
    );
}

/// The timeline information carries the members the game's scripts read.
///
/// The reference publishes one entry per playing timeline, and every entry
/// carries the timeline's label, its flags, its loop and playing state, the
/// current time, the total frame count and the blend ratio
/// (`reference/cpp/plugins/motionplayer/PlayerInternal.h:347-364`,
/// `reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:336-339`). The
/// game's `AffineSourceMotion` reads `label` and `blendRatio` from the
/// entries, so a missing member ends the scenario with a fatal script error.
#[test]
fn the_timeline_information_carries_the_reference_members() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    // The motion player's classes exist only after its first link, the way
    // a game script links it.
    env.link_plugin("motionplayer.dll");
    let dir = env.game_dir().expect("the temp game directory").clone();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference/tests/test_files/emote")
        .join(MOTION_FIXTURE);
    std::fs::copy(&fixture, dir.join(MOTION_FIXTURE)).expect("copy the motion fixture");
    env.run(&format!(
        "Motion.ResourceManager.setEmotePSBDecryptSeed({MOTION_SEED});\
         var rm = new Motion.ResourceManager(null, 1024);\
         var m = rm.load(\"{MOTION_FIXTURE}\");\
         var p = new Motion.Player(rm);\
         p.project = m;\
         var label = p.getMainTimelineLabelAt(0);\
         p.playTimeline(label, 1);\
         var info = p.getPlayingTimelineInfoList();"
    ));
    // The fixture declares its timelines, so one of them plays and the
    // information list holds an entry for it
    // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:207-210`).
    assert!(env.eval_int("info.count") >= 1);
    // The entry names the timeline that is playing
    // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:357`).
    env.run("var sameLabel = (info[0].label === label) ? 1 : 0;");
    assert_eq!(env.eval_int("sameLabel"), 1);
    // The remaining members of the entry are published as well
    // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:358-363`).
    env.run(
        "var members = [\"flags\", \"loop\", \"playing\", \"currentTime\", \
         \"totalFrames\", \"blendRatio\"];\
         var published = 0;\
         for (var i = 0; i < members.count; i++) {\
             if (typeof info[0][members[i]] != \"undefined\") { published++; }\
         }",
    );
    assert_eq!(env.eval_int("published"), 6);
}

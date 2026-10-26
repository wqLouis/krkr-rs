//! Integration tests for the motion player's timing semantics.
//!
//! The plugin's clock is the asset's own frame time counted at sixty frames a
//! second, and every `progress` call advances that clock by the whole delta
//! in one step: the reference adds the delta to the playing timeline's
//! current time (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:468`)
//! and evaluates the node tree once, at the new time
//! (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:871`,
//! `reference/cpp/plugins/motionplayer/PlayerUpdateLayers.cpp:63`). A host
//! that updates the player late therefore shows the pose at the new time; the
//! poses in between are not presented one by one.
//!
//! The tests drive the shipped script surface: a `Motion.Player` bound to the
//! reference's e-mote fixture, whose `全体構造` clip is a one-shot of 61
//! frames (`lastTime 61`, `loopTime -1`).

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// The reference's e-mote module.
const FIXTURE: &str = "e-mote3.0バニラパジャマa.psb";

/// The one-shot clip of that module: 61 frames, no loop.
const CLIP: &str = "全体構造";

/// The length of that clip in frames.
const CLIP_FRAMES: f64 = 61.0;

/// A bound VM + host + the reference fixture directory mounted as storage.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine, scene and storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _storage: Box<Storage>,
}

impl Env {
    /// A VM whose storage is the reference `emote` fixture directory.
    fn new() -> Env {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/tests/test_files/emote");
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(&dir),
                dir.clone(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage outlive the host binding (they
        // are declared after `_host` and dropped later).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "motion-timing-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.engine.eval(expression, "motion-timing-test") {
            Ok(TjsValue::Real(value)) => value,
            Ok(TjsValue::Integer(value)) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.engine.eval(expression, "motion-timing-test") {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Load the fixture and start a player named `name` on the one-shot
    /// clip.
    fn start(&self, name: &str) {
        self.run(&format!(
            "Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);\n\
             var rm_{name} = new Motion.ResourceManager(null, 1024);\n\
             var m_{name} = rm_{name}.load(\"{FIXTURE}\");\n\
             var {name} = new Motion.Player(rm_{name});\n\
             {name}.project = m_{name};\n\
             {name}.play(\"{CLIP}\", 0);\n"
        ));
    }

    /// The current time of the player's playing timeline, in frames.
    fn timeline_time(&self, player: &str, info: &str) -> f64 {
        self.run(&format!(
            "var {info} = {player}.getPlayingTimelineInfoList();"
        ));
        self.eval_real(&format!("{info}[0].currentTime"))
    }

    /// The number of playing timelines.
    fn playing_count(&self, player: &str, info: &str) -> i64 {
        self.run(&format!(
            "var {info} = {player}.getPlayingTimelineInfoList();"
        ));
        self.eval_int(&format!("{info}.count"))
    }
}

/// A millisecond counts at sixty frames a second, and the delta is added to
/// the playing timeline's current time
/// (`reference/cpp/plugins/motionplayer/PlayerInternal.h:174`,
/// `reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:468`,
/// `reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:971`).
#[test]
fn a_delta_counts_at_sixty_frames_a_second() {
    let _lock = vm_lock();
    let env = Env::new();
    env.start("p1");
    // The tick that follows the play is swallowed by the frame gate
    // (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:819-826`).
    env.run("p1.progress(0);");
    env.run("p1.progress(100);");
    assert_eq!(
        env.eval_real("p1.frameTickCount"),
        6.0,
        "100 ms is six frames"
    );
    assert_eq!(
        env.timeline_time("p1", "info1"),
        6.0,
        "the clock of the playing timeline is six frames"
    );
}

/// One update advances the clock by the whole delta. The reference adds the
/// delta to the current time in one step and evaluates the tree once, at the
/// new time (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:468`,
/// `reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:871`,
/// `reference/cpp/plugins/motionplayer/PlayerUpdateLayers.cpp:63`), so a
/// single late update lands on the pose of the new time and not on the pose
/// of the first step.
#[test]
fn one_update_advances_the_clock_by_the_whole_delta() {
    let _lock = vm_lock();
    let env = Env::new();
    env.start("p1");
    env.run("p1.progress(0);");
    env.run("p1.progress(100);");
    env.start("p2");
    env.run("p2.progress(0);");
    env.run("p2.progress(100.0 / 6.0);");
    let jumped = env.timeline_time("p1", "info1");
    let stepped = env.timeline_time("p2", "info2");
    assert!(
        (jumped - 6.0).abs() < 1e-9,
        "the single update advanced the clock by the whole delta: {jumped}"
    );
    assert!(
        (stepped - 1.0).abs() < 1e-9,
        "one sixth of the delta advances the clock by one frame: {stepped}"
    );
}

/// Six updates of a sixth of the delta end at the same clock as one update of
/// the whole delta: the reference neither sub-steps the clock nor caps the
/// total it applies
/// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:468`).
#[test]
fn six_updates_end_at_the_same_clock_as_one() {
    let _lock = vm_lock();
    let env = Env::new();
    env.start("p1");
    env.run("p1.progress(0);");
    env.run("p1.progress(100);");
    env.start("p2");
    env.run("p2.progress(0);");
    for _ in 0..6 {
        env.run("p2.progress(100.0 / 6.0);");
    }
    let jumped = env.timeline_time("p1", "info1");
    let stepped = env.timeline_time("p2", "info2");
    assert!(
        (jumped - stepped).abs() < 1e-9,
        "the clock does not depend on how the delta was delivered: {jumped} against {stepped}"
    );
}

/// A one-shot clip stops at its last frame when one update overshoots it: the
/// reference adds the delta, and the wrap that cannot restart the clip ends
/// the timeline at the clip's length
/// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:468-476`,
/// `reference/cpp/plugins/motionplayer/RuntimeSupport.h:421-443`). The
/// update does not leave the clip running and it does not restart it.
#[test]
fn a_one_shot_clip_stops_at_its_last_frame() {
    let _lock = vm_lock();
    let env = Env::new();
    env.start("p1");
    env.run("p1.progress(0);");
    env.run("var before = p1.getPlayingTimelineInfoList();");
    assert_eq!(
        env.eval_real("before[0].totalFrames"),
        CLIP_FRAMES,
        "the clip is a one-shot of 61 frames"
    );
    // 2000 ms is 120 frames, past the clip's 61.
    env.run("p1.progress(2000);");
    assert_eq!(
        env.playing_count("p1", "after"),
        0,
        "the overshooting update stopped the one-shot clip"
    );
    assert_eq!(
        env.eval_int("p1.playing ? 1 : 0"),
        0,
        "the player reports that nothing plays any more"
    );
}

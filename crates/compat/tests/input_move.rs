//! The mouse-move path: the two rules that bound it, and its cost.
//!
//! The measurement drives the real L3 event adapter
//! ([compat::EventAdapter::dispatch_frame]) against a synthetic scene, so
//! the numbers are the port's own cost and not the game's.
//!
//! The reference behaviour the measurement compares against:
//!
//! * the layer's move event fires only when the pointer position changed
//!   (reference/cpp/core/visual/LayerManager.cpp:426 computes poschanged,
//!   reference/cpp/core/visual/LayerManager.cpp:507 guards the layer's
//!   FireMouseMove with it);
//! * the window's move event is posted discardable, so a pending move event
//!   is dropped when a newer one arrives
//!   (reference/cpp/core/visual/WindowIntf.cpp:361-372);
//! * the hit test runs on every move
//!   (reference/cpp/core/visual/LayerManager.cpp:433), and it fires
//!   onHitTest only for a layer that passed the geometric test
//!   (reference/cpp/core/visual/LayerIntf.cpp:3286-3302,
//!   reference/cpp/core/visual/LayerIntf.cpp:3311-3361).

mod common;

use std::time::{Duration, Instant};

use common::{Env, vm_lock};
use platform::{InputEvent, PlatformEvent};

/// The script that builds the measured scene.
///
/// count full-screen layers hang under the window's primary layer, so every
/// pointer position is inside all of them. ctor is the class the layers are
/// built from: Layer accepts the hit test, Veto declines it, so the search
/// continues behind every candidate. window_work and layer_work are the
/// number of loop turns the handlers do, which stands for the work a game's
/// own handler does.
fn scene_script(count: usize, ctor: &str, window_work: usize, layer_work: usize) -> String {
    let class = if ctor == "Layer" {
        String::new()
    } else {
        format!(
            "class {ctor} extends Layer {{ \
               function {ctor}(w, p) {{ super.Layer(w, p); }} \
               function onHitTest(x, y, hit) {{ hits = hits + 1; super.onHitTest(x, y, false); }} \
             }};"
        )
    };
    format!(
        concat!(
            "var hits = 0; var moves = 0; var wmoves = 0; var lastX = -1; var sum = 0; ",
            "var w = new Window(); ",
            "w.setSize(1280, 720); ",
            "w.onMouseMove = function(x, y, shift) {{ wmoves = wmoves + 1; lastX = x; ",
            "  for (var i = 0; i < {window_work}; i++) {{ sum = sum + i; }} }}; ",
            "var base = new Layer(w, null); ",
            "base.setSize(1280, 720); base.visible = true; ",
            "var keep = []; ",
            "{class} ",
            "for (var i = 0; i < {count}; i++) {{ ",
            "  var l = new {ctor}(w, base); ",
            "  l.setPos(0, 0); l.setSize(1280, 720); l.visible = true; l.hitThreshold = 0; ",
            "  l.onMouseMove = function(x, y, shift) {{ moves = moves + 1; ",
            "    for (var j = 0; j < {layer_work}; j++) {{ sum = sum + j; }} }}; ",
            "  keep[keep.length] = l; ",
            "}}"
        ),
        class = class,
        count = count,
        ctor = ctor,
        window_work = window_work,
        layer_work = layer_work,
    )
}

/// A burst of pointer moves, each at its own position.
fn moves(count: usize) -> Vec<PlatformEvent> {
    (0..count)
        .map(|index| {
            PlatformEvent::Input(InputEvent::PointerMoved {
                x: 100.0 + index as f32,
                y: 100.0,
            })
        })
        .collect()
}

/// The counts the scene's handlers recorded.
#[derive(Clone, Copy, Debug)]
struct Counts {
    /// The onHitTest deliveries.
    hits: i64,
    /// The layer's onMouseMove deliveries.
    moves: i64,
    /// The window's onMouseMove deliveries.
    window_moves: i64,
}

impl Counts {
    fn read(env: &Env) -> Counts {
        Counts {
            hits: env.eval_int("hits"),
            moves: env.eval_int("moves"),
            window_moves: env.eval_int("wmoves"),
        }
    }
}

/// The per-event cost of a mouse move against a scene of layers layers.
///
/// The scene is built and dropped inside the function so the process-global
/// VM binding is released between the configurations.
fn measure(
    layers: usize,
    ctor: &str,
    window_work: usize,
    layer_work: usize,
    events: usize,
) -> (Duration, Counts) {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&scene_script(layers, ctor, window_work, layer_work));
    let burst = moves(events);
    let started = Instant::now();
    env.dispatch_events(&burst);
    let elapsed = started.elapsed();
    let counts = Counts::read(&env);
    (elapsed, counts)
}

/// The cost of one mouse move against a growing scene.
///
/// The measurement prints its numbers: the per-event cost is the port's own
/// wall-clock cost for one dispatched move, and the counts say how many
/// script events one move delivered.
#[test]
fn the_mouse_move_cost_grows_with_the_layer_count() {
    println!("--- layers, one candidate (the front layer accepts the hit test) ---");
    println!("layers  us/move  hits/move  layer moves  window moves");
    for layers in [1usize, 8, 32, 64, 128, 256] {
        let (elapsed, counts) = measure(layers, "Layer", 0, 0, 200);
        let per_move = elapsed.as_secs_f64() * 1e6 / 200.0;
        println!(
            "{layers:>6}  {per_move:>7.2}  {:>9}  {:>11}  {:>12}",
            counts.hits as f64 / 200.0,
            counts.moves,
            counts.window_moves
        );
    }

    println!("--- layers, every candidate declines the hit test ---");
    println!("layers  us/move  hits/move  layer moves  window moves");
    for layers in [1usize, 8, 32, 64, 128] {
        let (elapsed, counts) = measure(layers, "Veto", 0, 0, 50);
        let per_move = elapsed.as_secs_f64() * 1e6 / 50.0;
        println!(
            "{layers:>6}  {per_move:>7.2}  {:>9}  {:>11}  {:>12}",
            counts.hits as f64 / 50.0,
            counts.moves,
            counts.window_moves
        );
    }
}

/// The cost of a storm of moves delivered in one frame.
///
/// The handler does a little work, so the numbers show what a game's own
/// handler costs when the port calls it once per move.
#[test]
fn the_cost_of_a_storm_of_moves() {
    for (window_work, layer_work) in [(0usize, 0usize), (0, 2000), (2000, 0)] {
        println!(
            "--- one frame, one burst, window handler {window_work} loop turns, \
             layer handler {layer_work} loop turns ---"
        );
        println!("events  total ms  us/event  hits  layer moves  window moves");
        for events in [50usize, 200, 1000] {
            let (elapsed, counts) = measure(64, "Layer", window_work, layer_work, events);
            println!(
                "{events:>6}  {:>8.2}  {:>8.2}  {:>4}  {:>11}  {:>12}",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1e6 / events as f64,
                counts.hits,
                counts.moves,
                counts.window_moves
            );
        }
    }
}

/// The cost of a storm the dispatch budget cuts short.
///
/// The budget is wall-clock time, so a frame that collected a burst stops
/// dispatching and leaves the rest queued. The measurement prints how many
/// frames the burst took and what the replay cost.
#[test]
fn the_cost_of_replaying_a_deferred_burst() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&scene_script(64, "Layer", 0, 0));
    let burst = moves(1000);
    // The default budget: a quarter of a 60 fps frame.
    let mut adapter = compat::EventAdapter::new();
    let mut frames = 0usize;
    let started = Instant::now();
    let mut pending = burst.as_slice();
    loop {
        env.dispatch_with(&mut adapter, pending);
        frames += 1;
        pending = &[];
        if frames > 100 || env.eval_int("moves") >= 1000 {
            break;
        }
    }
    let elapsed = started.elapsed();
    println!(
        "--- 1000 moves, the default dispatch budget ---\
         \nframes: {frames}  total: {:.2} ms  per move: {:.2} us",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1e6 / 1000.0
    );
    assert_eq!(env.eval_int("moves"), 1000, "every move reached the layer");
}

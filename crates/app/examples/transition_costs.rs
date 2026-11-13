//! The cost of a transition's two endpoint composites and of its CPU blend.
//!
//! The gameplay harness measures the transition's whole per-tick cost. This
//! one measures the two parts of it, on a layer tree shaped like the game's
//! transition endpoints: a 640×480 layer with seventeen contributors, the
//! shape the `MEASURE-DIRTY` lines of the game's run reported.
//!
//! The endpoint composite is `engine::layer_ops::render_layer_subtree` — the
//! CPU composite of a layer's whole subtree into a content bitmap. The CPU
//! blend is `engine::trans::Trans::apply` — the cross-fade the null backend
//! writes into the transition's display bitmap.
//!
//! Run: cargo run -p app --example transition_costs -- [--width 640] [--height 480] \
//!       [--layers 17] [--rounds 20]

use std::time::Instant;

use engine::scene::Scene;
use engine::trans::Trans;

/// The width of one layer, in pixels.
const WIDTH: u32 = 640;
/// The height of one layer, in pixels.
const HEIGHT: u32 = 480;
/// The number of contributors under the root layer.
const LAYERS: usize = 16;

/// The sizes of the contributors, as the game's `MEASURE-DIRTY` lines
/// reported them: four full-size images, one 480×320 and the rest small.
const SIZES: [(u32, u32); 16] = [
    (640, 480),
    (336, 192),
    (16, 16),
    (88, 97),
    (88, 97),
    (640, 480),
    (336, 192),
    (104, 98),
    (104, 98),
    (336, 192),
    (16, 16),
    (480, 320),
    (640, 480),
    (16, 16),
    (16, 16),
    (16, 16),
];
/// The number of measured rounds.
const ROUNDS: usize = 20;

/// Reads one command-line argument as a number.
///
/// A missing argument is the default; a malformed one is an error the run
/// reports and leaves on.
fn number<T>(args: &mut impl Iterator<Item = String>, name: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match args.next() {
        Some(value) => match value.parse() {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("the {name} is not a number: {error}");
                std::process::exit(2);
            }
        },
        None => default,
    }
}

/// Fills a bitmap with a gradient, so the blend has something to mix.
fn pixels(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x + seed) as u8,
                (y + seed) as u8,
                ((x + y) as u8).wrapping_add(seed as u8),
                255,
            ]);
        }
    }
    rgba
}

/// Builds a layer tree shaped like the game's transition endpoints.
///
/// The root layer holds the content bitmap the composite writes into, and
/// every contributor under it holds an image of its own.
fn scene(width: u32, height: u32, layers: usize) -> (Scene, u32) {
    let mut scene = Scene::default();
    let window = scene.add_window("transition-cost", (1280, 720));
    let root = scene.add_layer(window, None);
    {
        let layer = scene.layer_mut(root).expect("the root layer");
        layer.rect = engine::scene::Rect::new(0, 0, width, height);
        layer.visible = true;
        layer.opacity = 1.0;
    }
    for index in 0..layers {
        let (layer_width, layer_height) = SIZES[index % SIZES.len()];
        let id = scene.add_layer(window, Some(root));
        let bitmap = scene.add_bitmap(
            layer_width,
            layer_height,
            pixels(layer_width, layer_height, index as u32),
        );
        let layer = scene.layer_mut(id).expect("the contributor layer");
        layer.rect = engine::scene::Rect::new(0, 0, layer_width, layer_height);
        layer.visible = true;
        layer.opacity = 1.0;
        layer.bitmap = Some(bitmap);
    }
    (scene, root)
}

/// Measures the endpoint composite: the CPU composite of a layer's subtree.
///
/// Every round invalidates the tree's content first, so the round measures a
/// composite and not the signature check that skips it.
fn composite_cost(scene: &mut Scene, root: u32, rounds: usize) -> f64 {
    let mut elapsed = 0.0;
    for _ in 0..rounds {
        // The content the last composite wrote is stale now: the tree moved.
        let contributors: Vec<u32> = scene
            .layer(root)
            .map(|layer| layer.children.clone())
            .unwrap_or_default();
        for id in contributors {
            if let Some(layer) = scene.layer_mut(id) {
                layer.content_epoch = layer.content_epoch.wrapping_add(1);
            }
        }
        let started = Instant::now();
        engine::layer_ops::render_layer_subtree(scene, root);
        elapsed += started.elapsed().as_secs_f64() * 1000.0;
    }
    elapsed / rounds as f64
}

/// Measures the CPU blend: the cross-fade into the destination bitmap.
fn blend_cost(scene: &mut Scene, rounds: usize) -> f64 {
    let width = WIDTH;
    let height = HEIGHT;
    let source = scene.add_bitmap(width, height, pixels(width, height, 1));
    let other = scene.add_bitmap(width, height, pixels(width, height, 2));
    let target = scene.add_bitmap(width, height, pixels(width, height, 3));
    let mut trans = Trans::cross_fade(1000.0).expect("the cross-fade");
    // The blend runs in the middle of the fade, where it mixes the two
    // sources; at the ends it copies one of them.
    trans.seek_ms(500.0);
    let mut elapsed = 0.0;
    for _ in 0..rounds {
        let started = Instant::now();
        let region = Trans::full_region(scene.bitmap(target).expect("the target"));
        let result = {
            let (target, source, other) = scene
                .bitmap_mut_with_refs(target, source, other)
                .expect("the three bitmaps");
            trans.apply(target, source, other, region)
        };
        result.expect("the blend");
        elapsed += started.elapsed().as_secs_f64() * 1000.0;
    }
    elapsed / rounds as f64
}

fn main() {
    let mut width = WIDTH;
    let mut height = HEIGHT;
    let mut layers = LAYERS;
    let mut rounds = ROUNDS;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--width" => width = number(&mut args, "width", WIDTH),
            "--height" => height = number(&mut args, "height", HEIGHT),
            "--layers" => layers = number(&mut args, "layer count", LAYERS),
            "--rounds" => rounds = number(&mut args, "round count", ROUNDS),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let (mut scene, root) = scene(width, height, layers);
    println!("the tree: a {width}x{height} layer with {layers} contributors");
    let composite = composite_cost(&mut scene, root, rounds);
    println!("the endpoint composite: {composite:.2} ms");
    let blend = blend_cost(&mut scene, rounds);
    println!("the CPU cross-fade blend: {blend:.2} ms");
    println!(
        "a transition tick on the null backend: {:.2} ms",
        2.0 * composite + blend
    );
    println!("a transition tick with the endpoints composited once per transition: {blend:.2} ms");
}

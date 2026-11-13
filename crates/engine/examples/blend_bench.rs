//! The cost of one blend pass, measured directly.
//!
//! The game showed the mode-2 blend costing 201 ms for a 1280x960 area -
//! 65 ns per pixel - while a mode-1 copy of the same area took 7.6 ms.
//! This measures the pass itself, so the number does not depend on the game.

use std::time::Instant;

use engine::layer_ops::{BlitOptions, BlitSource, blit_over, operate_rect};
use engine::scene::BitmapState;

fn plane(width: u32, height: u32, alpha: u8) -> BitmapState {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x * 7 % 251) as u8,
                (y * 13 % 241) as u8,
                ((x + y) * 3 % 239) as u8,
                alpha,
            ]);
        }
    }
    BitmapState::new(width, height, rgba)
}

fn main() {
    let width = 1280u32;
    let height = 960u32;
    let src = plane(width, height, 128);
    let mut dst = plane(width, height, 255);

    // The mode-1 pass: the plain source-over, the row-based one.
    let started = Instant::now();
    blit_over(
        &mut dst,
        0,
        0,
        BlitSource {
            bitmap: &src,
            rect: (0, 0, width as i32, height as i32),
        },
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        BlitOptions {
            mode: 0,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
            source_over: false,
        },
    );
    let source_over_ms = started.elapsed().as_secs_f64() * 1000.0;

    // The mode-2 pass: the alpha source-over, the one the subtree composite
    // runs for every `ltAlpha` layer.
    let mut dst2 = plane(width, height, 255);
    let started = Instant::now();
    operate_rect(
        &mut dst2,
        0,
        0,
        BlitSource {
            bitmap: &src,
            rect: (0, 0, width as i32, height as i32),
        },
        BlitOptions {
            mode: 2,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
            source_over: false,
        },
    );
    let alpha_ms = started.elapsed().as_secs_f64() * 1000.0;

    let pixels = (width * height) as f64;
    println!("the area: {width}x{height} = {pixels:.0} pixels");
    println!(
        "the source-over: {source_over_ms:.1} ms = {:.1} ns per pixel",
        source_over_ms * 1e6 / pixels
    );
    println!(
        "the alpha source-over: {alpha_ms:.1} ms = {:.1} ns per pixel",
        alpha_ms * 1e6 / pixels
    );
    println!(
        "the ratio: {:.1}x",
        alpha_ms / source_over_ms.max(f64::MIN_POSITIVE)
    );
}

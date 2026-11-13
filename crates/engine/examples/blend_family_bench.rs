//! The cost of every blend the engine can run, measured per pixel.
//!
//! The game showed a mode-2 draw over 1280x960 costing 201 ms - 65 ns per
//! pixel - while a mode-1 copy of the same area took 7.6 ms. This walks the
//! whole blend family: every operation mode the engine maps, every
//! destination face it supports, every fill and every per-pixel native, and
//! reports the cost per pixel next to the mode-1 copy, which is the memory
//! floor for the same bytes, and next to the row-based source-over, which is
//! the engine's own fastest per-pixel blend.
//!
//! The source plane carries a *mix* of alpha values: the fast paths
//! special-case a wholly opaque and a wholly transparent source, and a mixed
//! source is what the game's layers actually hold.
//!
//! ```text
//! cargo build -p engine --release --example blend_family_bench
//! ./target/release/examples/blend_family_bench
//! ```

use std::time::Instant;

use engine::layer_ops::{
    BlitOptions, BlitSource, COPY_MAIN, COPY_MASK, GammaAdjust, adjust_gamma, blit_copy_clipped,
    blit_over, blit_plane, blur_bitmap_in_place, colorize, convert_add_alpha_to_alpha,
    convert_alpha_to_add_alpha, do_gray_scale, fill_color_hold_alpha, fill_color_on_add_alpha,
    fill_color_on_alpha, fill_mask, light_contrast, noise, operate_rect_on, remove_const_opacity,
    stretch_blit_mode, stretch_blit_on, tile_rect,
};
use engine::scene::{BitmapState, Rect};
use engine::trans::DestAlpha;

/// The rectangle every kernel is measured over.
const AREA_W: u32 = 512;
const AREA_H: u32 = 512;

/// The operation modes the engine maps (`blend_operation_mode`).
const MODES: &[i64] = &[
    1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29,
];

/// The destination faces the engine distinguishes.
const FACES: &[(&str, DestAlpha)] = &[
    ("Straight", DestAlpha::Straight),
    ("Additive", DestAlpha::Additive),
    ("Opaque", DestAlpha::Opaque),
];

/// A source plane with a mix of alpha values.
fn source_plane(width: u32, height: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.push((x * 61 + y * 29) as u8);
            rgba.push((x * 13 + y * 47) as u8);
            rgba.push((x * 29 + y * 71) as u8);
            // The alpha sweeps the whole range, so the source is neither
            // wholly opaque nor wholly transparent.
            rgba.push((x * 61 + y * 29) as u8);
        }
    }
    BitmapState::new(width, height, rgba)
}

/// A destination plane with a mix of alpha values.
fn destination_plane(width: u32, height: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.push((x * 37 + y * 11) as u8);
            rgba.push((x * 19 + y * 53) as u8);
            rgba.push((x * 43 + y * 7) as u8);
            rgba.push((x * 17 + y * 23) as u8);
        }
    }
    BitmapState::new(width, height, rgba)
}

/// Time `run` over `pixels` pixels and report the best run's nanoseconds per
/// pixel.
///
/// The best run is the measurement: a scheduled-out iteration only adds noise
/// to the cost of the kernel itself.
fn time_per_pixel(pixels: u64, mut run: impl FnMut()) -> f64 {
    // One warm-up run, so the estimate below is not the cold-cache run.
    run();
    let started = Instant::now();
    run();
    let estimate = started.elapsed().as_secs_f64();
    // Aim for about 60 ms of work per measurement, so the best run below is
    // picked from enough samples to reject a scheduled-out iteration.
    let iterations = ((0.060 / estimate.max(1e-9)) as usize).clamp(7, 400);
    let mut best = f64::INFINITY;
    for _ in 0..iterations {
        let started = Instant::now();
        run();
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed < best {
            best = elapsed;
        }
    }
    best * 1e9 / pixels as f64
}

/// One measured kernel.
struct Row {
    /// What was measured.
    label: String,
    /// Nanoseconds per pixel.
    ns_per_pixel: f64,
    /// Whether the pass changed the destination at all.
    changed: bool,
}

/// The rows of one plane's table.
struct Table {
    rows: Vec<Row>,
}

impl Table {
    fn new() -> Self {
        Table { rows: Vec::new() }
    }

    /// Measure one kernel and record its row.
    ///
    /// The pass is measured on `destination`, which the earlier kernels have
    /// already written to. Whether the kernel has an implementation at all is
    /// decided on a copy of the pristine destination, so a kernel that
    /// repeats the previous one's result is not mistaken for a missing one.
    fn measure(
        &mut self,
        label: &str,
        pixels: u64,
        pristine: &BitmapState,
        destination: &mut BitmapState,
        kernel: impl Fn(&mut BitmapState),
    ) {
        let mut probe = pristine.clone();
        kernel(&mut probe);
        let changed = probe.rgba != pristine.rgba;
        let ns_per_pixel = time_per_pixel(pixels, || kernel(destination));
        self.rows.push(Row {
            label: label.to_string(),
            ns_per_pixel,
            changed,
        });
    }

    /// Print the table, with every row's ratio to the mode-1 copy and to the
    /// row-based source-over.
    fn print(&self, title: &str) {
        println!("\n=== {title} ===");
        let copy = self
            .rows
            .iter()
            .find(|row| row.label.starts_with("blit_copy_clipped"))
            .map_or(0.0, |row| row.ns_per_pixel);
        let over = self
            .rows
            .iter()
            .find(|row| row.label.starts_with("blit_over"))
            .map_or(0.0, |row| row.ns_per_pixel);
        println!(
            "{:<46} {:>10} {:>10} {:>10}",
            "kernel", "ns/px", "vs copy", "vs over"
        );
        println!("{}", "-".repeat(90));
        for row in &self.rows {
            let note = if row.changed {
                ""
            } else {
                "  [no-op: the engine has no kernel]"
            };
            println!(
                "{:<46} {:>10.3} {:>9.1}x {:>8.1}x{}",
                row.label,
                row.ns_per_pixel,
                ratio(row.ns_per_pixel, copy),
                ratio(row.ns_per_pixel, over),
                note
            );
        }
    }
}

/// The ratio of two costs, or zero when the base is zero.
fn ratio(value: f64, base: f64) -> f64 {
    if base > 0.0 { value / base } else { 0.0 }
}

/// The options a mode pass runs with.
fn options(mode: i64, opa: u8) -> BlitOptions {
    BlitOptions {
        mode,
        opa,
        treat_as_opaque: false,
        hold_alpha: false,
        source_over: false,
    }
}

fn main() {
    println!("blend family benchmark");
    println!(
        "area: {AREA_W}x{AREA_H} = {} pixels, best of N runs",
        AREA_W as u64 * AREA_H as u64
    );
    for (width, height) in [(1920u32, 1080u32), (1280u32, 960u32)] {
        run_plane(width, height);
    }
}

/// Measure every kernel over one plane.
fn run_plane(width: u32, height: u32) {
    let source = source_plane(width, height);
    let pristine = destination_plane(width, height);
    let mut destination = pristine.clone();
    let pixels = AREA_W as u64 * AREA_H as u64;
    let x0 = 0i32;
    let y0 = 0i32;
    let area = (x0, y0, x0 + AREA_W as i32, y0 + AREA_H as i32);
    let full_clip = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);

    let mut table = Table::new();

    // The mode-1 copy: the memory floor for the same bytes.
    table.measure(
        "blit_copy_clipped (mode-1 copy)",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            blit_copy_clipped(dst, x0, y0, &source, area, full_clip);
        },
    );

    // The plain source-over: the row-based band pass.
    table.measure(
        "blit_over (plain source-over)",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            blit_over(
                dst,
                x0,
                y0,
                BlitSource {
                    bitmap: &source,
                    rect: area,
                },
                full_clip,
                BlitOptions {
                    mode: 0,
                    opa: 255,
                    treat_as_opaque: false,
                    hold_alpha: false,
                    source_over: false,
                },
            );
        },
    );

    // `operate_rect` with the plain source-over selected by the flag.
    table.measure(
        "operate_rect (source_over = true)",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            operate_rect_on(
                DestAlpha::Straight,
                dst,
                x0,
                y0,
                BlitSource {
                    bitmap: &source,
                    rect: area,
                },
                BlitOptions {
                    mode: 0,
                    opa: 255,
                    treat_as_opaque: false,
                    hold_alpha: false,
                    source_over: true,
                },
            );
        },
    );

    // Every operation mode, on every destination face.
    for mode in MODES {
        for (face_name, face) in FACES {
            let label = format!("operate_rect mode={mode} face={face_name}");
            table.measure(&label, pixels, &pristine, &mut destination, |dst| {
                operate_rect_on(
                    *face,
                    dst,
                    x0,
                    y0,
                    BlitSource {
                        bitmap: &source,
                        rect: area,
                    },
                    options(*mode, 255),
                );
            });
        }
    }

    // The mode-1 copy read as an opaque image: a different kernel per face.
    for (face_name, face) in FACES {
        let label = format!("operate_rect mode=1 opaque-source face={face_name}");
        table.measure(&label, pixels, &pristine, &mut destination, |dst| {
            operate_rect_on(
                *face,
                dst,
                x0,
                y0,
                BlitSource {
                    bitmap: &source,
                    rect: area,
                },
                BlitOptions {
                    mode: 1,
                    opa: 255,
                    treat_as_opaque: true,
                    hold_alpha: false,
                    source_over: false,
                },
            );
        });
    }

    // The stretch passes. The source is a quarter of the destination, so the
    // resampler runs; the destination keeps the measured area.
    let stretch_source = (x0, y0, x0 + AREA_W as i32 / 2, y0 + AREA_H as i32 / 2);
    for mode in MODES {
        for (face_name, face) in FACES {
            let label = format!("stretch_blit mode={mode} face={face_name}");
            table.measure(&label, pixels, &pristine, &mut destination, |dst| {
                stretch_blit_on(
                    *face,
                    dst,
                    area,
                    BlitSource {
                        bitmap: &source,
                        rect: stretch_source,
                    },
                    0,
                    options(*mode, 255),
                );
            });
        }
    }

    // The stretch pass the game runs: a one-to-one transfer.
    table.measure(
        "stretch_blit_mode mode=1 (1:1)",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            stretch_blit_mode(
                dst,
                area,
                BlitSource {
                    bitmap: &source,
                    rect: area,
                },
                0,
                options(1, 255),
            );
        },
    );

    // The fills.
    let color = [32u8, 64, 96, 200];
    table.measure(
        "fill_color_on_alpha opa=128",
        pixels,
        &pristine,
        &mut destination,
        |dst| fill_color_on_alpha(dst, area, color, 128),
    );
    table.measure(
        "fill_color_on_alpha opa=255",
        pixels,
        &pristine,
        &mut destination,
        |dst| fill_color_on_alpha(dst, area, color, 255),
    );
    table.measure(
        "fill_color_on_add_alpha opa=128",
        pixels,
        &pristine,
        &mut destination,
        |dst| fill_color_on_add_alpha(dst, area, color, 128),
    );
    table.measure(
        "fill_color_hold_alpha opa=128",
        pixels,
        &pristine,
        &mut destination,
        |dst| fill_color_hold_alpha(dst, area, color, 128),
    );
    table.measure(
        "fill_color_hold_alpha opa=255",
        pixels,
        &pristine,
        &mut destination,
        |dst| fill_color_hold_alpha(dst, area, color, 255),
    );
    table.measure("fill_mask", pixels, &pristine, &mut destination, |dst| {
        fill_mask(dst, area, 128)
    });
    table.measure(
        "remove_const_opacity level=64",
        pixels,
        &pristine,
        &mut destination,
        |dst| remove_const_opacity(dst, area, 64),
    );

    // The other per-pixel natives.
    table.measure("colorize", pixels, &pristine, &mut destination, |dst| {
        colorize(dst, area, 200, 128, 0.5)
    });
    table.measure("adjust_gamma", pixels, &pristine, &mut destination, |dst| {
        adjust_gamma(
            dst,
            area,
            GammaAdjust {
                r_gamma: 2.2,
                r_floor: 0,
                r_ceil: 255,
                g_gamma: 2.2,
                g_floor: 0,
                g_ceil: 255,
                b_gamma: 2.2,
                b_floor: 0,
                b_ceil: 255,
            },
        );
    });
    table.measure(
        "light_contrast",
        pixels,
        &pristine,
        &mut destination,
        |dst| light_contrast(dst, area, 10, 20),
    );
    table.measure(
        "convert_alpha_to_add_alpha",
        pixels,
        &pristine,
        &mut destination,
        |dst| convert_alpha_to_add_alpha(dst, area),
    );
    table.measure(
        "convert_add_alpha_to_alpha",
        pixels,
        &pristine,
        &mut destination,
        |dst| convert_add_alpha_to_alpha(dst, area),
    );
    table.measure(
        "do_gray_scale",
        pixels,
        &pristine,
        &mut destination,
        |dst| do_gray_scale(dst, area),
    );
    table.measure(
        "noise level=32",
        pixels,
        &pristine,
        &mut destination,
        |dst| noise(dst, area, 32, 7),
    );

    // The plane copies.
    table.measure(
        "blit_plane COPY_MAIN|COPY_MASK",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            blit_plane(dst, x0, y0, &source, area, full_clip, COPY_MAIN | COPY_MASK);
        },
    );
    table.measure(
        "blit_plane COPY_MAIN",
        pixels,
        &pristine,
        &mut destination,
        |dst| blit_plane(dst, x0, y0, &source, area, full_clip, COPY_MAIN),
    );

    // The tile.
    let tile = source_plane(64, 64);
    table.measure(
        "tile_rect 64x64 tile",
        pixels,
        &pristine,
        &mut destination,
        |dst| {
            tile_rect(
                dst,
                Rect::new(x0, y0, AREA_W, AREA_H),
                &tile,
                (0, 0, 64, 64),
                (0, 0),
            );
        },
    );

    table.print(&format!("plane {width}x{height}"));

    // The whole plane, so the numbers can be read next to the game's.
    let plane_pixels = u64::from(width) * u64::from(height);
    let plane_area = (0i32, 0i32, width as i32, height as i32);
    let mut wide = Table::new();
    wide.measure(
        "blit_copy_clipped (mode-1 copy)",
        plane_pixels,
        &pristine,
        &mut destination,
        |dst| {
            blit_copy_clipped(dst, 0, 0, &source, plane_area, full_clip);
        },
    );
    wide.measure(
        "operate_rect mode=2 face=Straight",
        plane_pixels,
        &pristine,
        &mut destination,
        |dst| {
            operate_rect_on(
                DestAlpha::Straight,
                dst,
                0,
                0,
                BlitSource {
                    bitmap: &source,
                    rect: plane_area,
                },
                options(2, 255),
            );
        },
    );
    wide.measure(
        "blit_over (plain source-over)",
        plane_pixels,
        &pristine,
        &mut destination,
        |dst| {
            blit_over(
                dst,
                0,
                0,
                BlitSource {
                    bitmap: &source,
                    rect: plane_area,
                },
                full_clip,
                BlitOptions {
                    mode: 0,
                    opa: 255,
                    treat_as_opaque: false,
                    hold_alpha: false,
                    source_over: false,
                },
            );
        },
    );
    wide.print(&format!("whole plane {width}x{height}"));

    // The blur works on the whole bitmap, so it gets its own measurement.
    let mut blurred = destination_plane(width, height);
    let ns = time_per_pixel(plane_pixels, || {
        blur_bitmap_in_place(&mut blurred, 4, 4);
    });
    println!("\nblur_bitmap_in_place r=4 over {width}x{height}: {ns:.3} ns/px");
}
